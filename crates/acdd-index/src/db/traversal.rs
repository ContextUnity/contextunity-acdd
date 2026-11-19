use super::{paging, reader, symbols};
use crate::db::commitments;
use acdd_core::config;
use acdd_core::git;
use crate::models::stable_hash64;
use acdd_core::response::{CoverageOptions, QueryOptions, MAX_PAGE_SIZE};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::{Component, Path},
};
// Stored decorates edges point from decorator to target; dependency walks reverse them.
pub(super) const FORWARD_DEPENDENCIES: &str =
    "'calls','possible_calls','inherits','implements','overrides','extends','includes','mutates','handles','references','calls_endpoint','defines_table','queries_table','mutates_table','references_table'";
pub(super) const REVERSE_DEPENDENCIES: &str = "'decorates'";
const STRUCTURAL_DEPENDENCIES: &str = ",'imports','contains','documents'";
const MAX_DEEP_TRAVERSAL_FRONTIER: usize = 1000;
const MAX_CHANGE_FILES: usize = 100;
const MAX_CHANGE_SEEDS: usize = 64;
const MAX_CHANGE_RESULT_NODES: usize = 10_000;
const MAX_GIT_DIFF_BYTES: usize = 8 * 1024 * 1024;

/// Bounds and filters a graph traversal.
pub struct TraversalOptions<'a> {
    /// Maximum graph depth.
    pub depth: u32,
    /// Whether to follow incoming relationships.
    pub inbound: bool,
    /// Optional relationship mode.
    pub mode: Option<&'a str>,
    /// Optional exact relationship kinds.
    pub edge_types: Option<&'a [String]>,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Bounds a generic graph query.
pub struct GraphQueryOptions<'a> {
    /// Maximum graph depth for traversal operations.
    pub depth: u32,
    /// Incoming or outgoing impact direction.
    pub direction: Option<&'a str>,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

fn removal_assessment(
    dependencies: usize,
    exact_dependencies: bool,
    target_unresolved: usize,
    target_errors: usize,
) -> Value {
    let mut reasons = Vec::new();
    if dependencies > 0 {
        reasons.push(if exact_dependencies {
            json!({"kind":"incoming_dependencies","scope":"selection","count":dependencies})
        } else {
            json!({"kind":"incoming_dependencies","scope":"selection","at_least":dependencies})
        });
    }
    if target_unresolved > 0 {
        reasons.push(
            json!({"kind":"unresolved_references","scope":"target","count":target_unresolved}),
        );
    }
    if target_errors > 0 {
        reasons.push(json!({"kind":"parse_errors","scope":"target","count":target_errors}));
    }
    json!({"verdict":if reasons.is_empty() {"no_indexed_blockers"} else {"blocked"},"blocking_reasons":reasons})
}

fn removal_diagnostics(conn: &Connection, selected_ids: &[String]) -> Result<(usize, usize)> {
    let ids = serde_json::to_string(selected_ids)?;
    let target_unresolved = paging::count(
        conn,
        "SELECT count(*) FROM resolution_coverage c JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN path_dictionary p ON p.path_id=c.path_id WHERE c.status IN('unresolved','ambiguous') AND (x.expression IN (SELECT n.name FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?1))) OR x.expression IN (SELECT n.qualname FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?1))) OR p.path IN (SELECT np.path FROM nodes n JOIN path_dictionary np ON np.path_id=n.path_id WHERE n.kind='module' AND n.id IN (SELECT value FROM json_each(?1))) OR (EXISTS(SELECT 1 FROM nodes n WHERE n.kind='module' AND n.id IN (SELECT value FROM json_each(?1))) AND p.path IN (SELECT ep.path FROM edges e JOIN path_dictionary ep ON ep.path_id=e.path_id JOIN nodes m ON m.node_hash=e.dst_hash JOIN path_dictionary mp ON mp.path_id=m.path_id WHERE e.kind='imports' AND mp.path IN (SELECT np.path FROM nodes n JOIN path_dictionary np ON np.path_id=n.path_id WHERE n.kind='module' AND n.id IN (SELECT value FROM json_each(?1))))))",
        &[&ids],
    )?;
    let target_errors = paging::count(
        conn,
        "SELECT count(*) FROM errors WHERE path IN (SELECT p.path FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id IN (SELECT value FROM json_each(?1)))",
        &[&ids],
    )?;
    Ok((target_unresolved, target_errors))
}

pub(super) fn immediate_links(
    conn: &Connection,
    id: &str,
    inbound: bool,
    structural: bool,
) -> Result<usize> {
    let (forward, reverse) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let extra = if structural {
        STRUCTURAL_DEPENDENCIES
    } else {
        ""
    };
    let forward = paging::count(
        conn,
        &format!("SELECT count(*) FROM edges WHERE {forward}=?1 AND kind IN({FORWARD_DEPENDENCIES}{extra})"),
        &[&stable_hash64(id)],
    )?;
    let reverse = paging::count(
        conn,
        &format!(
            "SELECT count(*) FROM edges WHERE {reverse}=?1 AND kind IN({REVERSE_DEPENDENCIES})"
        ),
        &[&stable_hash64(id)],
    )?;
    Ok(forward.saturating_add(reverse))
}

fn admit_traversal(conn: &Connection, id: &str, depth: u32, inbound: bool) -> Result<usize> {
    if depth <= 1 {
        return Ok(0);
    }
    let immediate = immediate_links(conn, id, inbound, true)?;
    if immediate > MAX_DEEP_TRAVERSAL_FRONTIER {
        bail!("selector {id} has {immediate} immediate graph links; depth {depth} would expand a broad graph before pagination. Retry with depth=1 and page the result, or select a narrower module or symbol. Reducing limit alone does not reduce traversal work");
    }
    Ok(immediate)
}

pub(super) fn resolve_traversal_kinds(
    mode: Option<&str>,
    edge_types: Option<&[String]>,
    structural: bool,
) -> (String, String) {
    if let Some(types) = edge_types {
        if !types.is_empty() {
            let mut forward = Vec::new();
            let mut reverse = Vec::new();
            for t in types {
                let lower = t.to_ascii_lowercase();
                if lower == "decorates" {
                    reverse.push(format!("'{lower}'"));
                } else {
                    forward.push(format!("'{lower}'"));
                }
            }
            let f_str = if forward.is_empty() {
                "'__none__'".into()
            } else {
                forward.join(",")
            };
            let r_str = reverse.join(",");
            return (f_str, r_str);
        }
    }

    match mode {
        Some("data-flow") | Some("dataflow") => {
            ("'mutates','references','handles'".into(), String::new())
        }
        Some("all") => (
            format!("{FORWARD_DEPENDENCIES}{STRUCTURAL_DEPENDENCIES}"),
            REVERSE_DEPENDENCIES.into(),
        ),
        _ => {
            let extra = if structural {
                STRUCTURAL_DEPENDENCIES
            } else {
                ""
            };
            (
                format!("{FORWARD_DEPENDENCIES}{extra}"),
                REVERSE_DEPENDENCIES.into(),
            )
        }
    }
}

pub(super) fn custom_dependency_steps(
    inbound: bool,
    bounded: bool,
    forward: &str,
    reverse: &str,
) -> String {
    let (from, to) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let depth = if bounded { ",w.depth+1" } else { "" };
    let guard = if bounded { "w.depth<?2 AND " } else { "" };
    if reverse.is_empty() {
        format!("SELECT e.{to}{depth} FROM walk w JOIN edges e ON e.{from}=w.id WHERE {guard}e.kind IN({forward})")
    } else {
        format!("SELECT e.{to}{depth} FROM walk w JOIN edges e ON e.{from}=w.id WHERE {guard}e.kind IN({forward}) UNION SELECT e.{from}{depth} FROM walk w JOIN edges e ON e.{to}=w.id WHERE {guard}e.kind IN({reverse})")
    }
}

pub use super::cycles::cycles;

/// Performs impact inbound.
pub fn impact_inbound(direction: Option<&str>) -> Result<bool> {
    match direction.unwrap_or("inbound") {
        "inbound" => Ok(true),
        "outbound" => Ok(false),
        _ => bail!("impact direction must be inbound or outbound"),
    }
}

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

fn parse_hunk_ranges(patch: &[u8]) -> Vec<ParsedHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<HunkState> = None;
    let text = String::from_utf8_lossy(patch);
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("@@ -") {
            if let Some(state) = current.take() {
                hunks.push(ParsedHunk {
                    added_range: state.added.then_some(state.added_range).flatten(),
                    deleted_only: state.removed && !state.added,
                });
            }
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
                if let Some(state) = current.take() {
                    hunks.push(ParsedHunk {
                        added_range: state.added.then_some(state.added_range).flatten(),
                        deleted_only: state.removed && !state.added,
                    });
                }
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
                if let Some(state) = current.take() {
                    hunks.push(ParsedHunk {
                        added_range: state.added.then_some(state.added_range).flatten(),
                        deleted_only: state.removed && !state.added,
                    });
                }
            }
        }
    }
    if let Some(state) = current {
        hunks.push(ParsedHunk {
            added_range: state.added.then_some(state.added_range).flatten(),
            deleted_only: state.removed && !state.added,
        });
    }
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
        "SELECT EXISTS(SELECT 1 FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 AND n.kind NOT IN('module','file') AND n.line<=?3 AND n.end_line>=?2)",
        rusqlite::params![path, i64::try_from(start)?, i64::try_from(end)?],
        |row| row.get(0),
    )?;
    Ok(found != 0)
}

fn worktree_postimage(root: &Path, adapter: &config::Adapter, path: &str) -> Result<Vec<u8>> {
    super::symbols::read_confined_source_bytes(
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
        anyhow::ensure!(
            valid_workspace_path(path),
            "path must be workspace-relative"
        );
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
    let graph_generation: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='output_root'",
        [],
        |row| row.get(0),
    )?;
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
            let adapter = worktree_adapter.get_or_insert_with(|| config::load_adapter(root, None).ok());
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

fn decode_edge_descriptor(value: Value) -> Result<Value> {
    match value {
        Value::String(encoded) => {
            serde_json::from_str(&encoded).context("invalid persisted impact edge descriptor")
        }
        other => Ok(other),
    }
}

fn changed_range_from_sql() -> &'static str {
    "json_each(?1) r JOIN path_dictionary p ON p.path=json_extract(r.value,'$.path') JOIN nodes n ON n.path_id=p.path_id AND n.kind NOT IN('module','file') AND n.line<=CAST(json_extract(r.value,'$.end') AS INTEGER) AND n.end_line>=CAST(json_extract(r.value,'$.start') AS INTEGER)"
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
    inbound: bool,
    forward: &str,
    reverse: &str,
) -> Result<usize> {
    let seed_json = serde_json::to_string(seed_hashes)?;
    let (from, to) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let reverse_edges = if reverse.is_empty() {
        String::new()
    } else {
        format!(" UNION ALL SELECT 1 FROM edges e JOIN json_each(?1) s ON e.{to}=CAST(s.value AS INTEGER) WHERE e.kind IN({reverse})")
    };
    paging::count(
        conn,
        &format!("SELECT count(*) FROM (SELECT 1 FROM edges e JOIN json_each(?1) s ON e.{from}=CAST(s.value AS INTEGER) WHERE e.kind IN({forward}){reverse_edges})"),
        &[&seed_json],
    )
}

fn changed_reachability_cte(steps: &str) -> String {
    format!(
        "WITH RECURSIVE walk(id,depth,visited) AS (SELECT CAST(value AS INTEGER),0,printf(',%lld,',CAST(value AS INTEGER)) FROM json_each(?1) UNION {steps} LIMIT {}), reached(id,distance) AS MATERIALIZED (SELECT id,min(depth) FROM walk GROUP BY id)",
        MAX_CHANGE_RESULT_NODES.saturating_add(1)
    )
}

fn changed_dependency_steps(inbound: bool, forward: &str, reverse: &str) -> String {
    let (from, to) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
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
    generation: &str,
) -> Result<Value> {
    if seed_hashes.is_empty() {
        return Ok(paging::value(Vec::new(), 0, request.page, generation));
    }
    let depth = request.depth;
    let (forward, reverse) = resolve_traversal_kinds(request.mode, request.edge_types, true);
    if depth > 0 {
        let immediate =
            changed_first_frontier(conn, seed_hashes, request.inbound, &forward, &reverse)?;
        if immediate > MAX_DEEP_TRAVERSAL_FRONTIER {
            bail!("Git change view has {immediate} immediate graph links; narrow the changed files or use depth=1 before paging");
        }
    }
    let seed_json = serde_json::to_string(seed_hashes)?;
    let steps = changed_dependency_steps(request.inbound, &forward, &reverse);
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
    let (from, to) = if request.inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    let reverse_edge = if reverse.is_empty() {
        String::new()
    } else {
        format!(" OR (e.{to} IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) AND e.{from}=n.node_hash AND e.kind IN({reverse}))")
    };
    let evidence = if options.detail == acdd_core::response::Detail::Full {
        ", 'evidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.evidence_id)"
    } else {
        ""
    };
    let edge = format!(
        "CASE WHEN r.distance=1 THEN (SELECT json_object('src_public_id',(SELECT id FROM nodes WHERE node_hash=e.src_hash),'dst_public_id',(SELECT id FROM nodes WHERE node_hash=e.dst_hash),'kind',e.kind,'path',(SELECT path FROM path_dictionary WHERE path_id=e.path_id),'line',e.line,'confidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.confidence_id),'occurrence_count',e.occurrence_count,'direction',CASE WHEN e.src_hash IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) THEN 'outbound' ELSE 'inbound' END{evidence}) FROM edges e WHERE ((e.{from} IN (SELECT CAST(value AS INTEGER) FROM json_each(?1)) AND e.{to}=n.node_hash AND e.kind IN({forward})){reverse_edge}) ORDER BY e.kind,e.src_hash,e.dst_hash,e.path_id,e.line,e.evidence_id LIMIT 1) END AS __edge"
    );
    let (forward_current, forward_predecessor, reverse_current, reverse_predecessor) =
        if request.inbound {
            ("src_hash", "dst_hash", "dst_hash", "src_hash")
        } else {
            ("dst_hash", "src_hash", "src_hash", "dst_hash")
        };
    let reverse_via = if reverse.is_empty() {
        String::new()
    } else {
        format!(" UNION ALL SELECT prior_node.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{reverse_predecessor} AND prior.distance=r.distance-1 JOIN nodes prior_node ON prior_node.node_hash=prior.id WHERE e.{reverse_current}=n.node_hash AND e.kind IN({reverse})")
    };
    let via = format!(
        "CASE WHEN r.distance>1 THEN (SELECT json_array(predecessor,edge_kind) FROM (SELECT prior_node.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_predecessor} AND prior.distance=r.distance-1 JOIN nodes prior_node ON prior_node.node_hash=prior.id WHERE e.{forward_current}=n.node_hash AND e.kind IN({forward}){reverse_via}) ORDER BY predecessor,edge_kind LIMIT 1) END AS __via"
    );
    let sql = format!("{cte} SELECT {columns},r.distance,{edge},{via} FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4");
    let mut items = reader::rows(
        conn,
        &sql,
        &[&seed_json, &depth, &limit, &offset],
        options.limit,
    )?;
    for item in &mut items {
        let fields = item.as_object_mut().context("invalid change impact node")?;
        let id = fields
            .get("id")
            .and_then(Value::as_str)
            .context("change impact node has no id")?;
        let is_seed = seed_ids.iter().any(|seed| seed == id);
        let edge = decode_edge_descriptor(fields.remove("__edge").unwrap_or(Value::Null))?;
        let via = fields
            .remove("__via")
            .and_then(|value| value.as_str().map(str::to_owned));
        fields.insert("is_seed".into(), json!(is_seed));
        if !is_seed && !edge.is_null() {
            fields.insert("edge".into(), edge);
        } else if !is_seed {
            if fields.get("distance").and_then(Value::as_u64) == Some(1) {
                bail!("depth-one change impact node has no persisted edge evidence");
            }
            let via = via
                .map(|encoded| serde_json::from_str::<(String, String)>(&encoded))
                .transpose()?
                .context("reached change impact node has no predecessor")?;
            fields.insert("via".into(), json!({"id":via.0,"kind":via.1}));
        }
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
            let page = symbols::tests_paged_with_path(conn, seed, None, "inbound", &page_options)?;
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

fn changed_impact_with_path_options(
    conn: &Connection,
    root: &Path,
    mode: &str,
    path_scope: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    if request.depth > 16 {
        bail!("depth must be <=16");
    }
    let snapshot = collect_change_snapshot(conn, root, mode, path_scope)?;
    let mut page = request.page.clone();
    let current_generation: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='output_root'",
        [],
        |row| row.get(0),
    )?;
    page.generation = Some(current_generation);
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
    Ok(json!({
        "selector":{"kind":"git_changes","mode":snapshot.mode},
        "direction":if request.inbound {"inbound"} else {"outbound"},
        "depth":request.depth,
        "mode":request.mode.unwrap_or("calls"),
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

/// Impact either one exact selector or the selected Git change view.
pub fn impact_with_path_options(
    conn: &Connection,
    root: &Path,
    selector: Option<&str>,
    changes: Option<&str>,
    path: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    let empty_selector = selector.is_some_and(|value| value.trim().is_empty());
    match (
        selector.filter(|value| !value.trim().is_empty()),
        changes.filter(|value| !value.trim().is_empty()),
    ) {
        (Some(selector), None) => {
            let mut result = traverse_with_path_options(conn, selector, path, request)?;
            let selected_id = result["selector"]["id"]
                .as_str()
                .or_else(|| result["selector"]["node"]["id"].as_str())
                .unwrap_or(selector)
                .to_owned();
            let selected_path = result["selector"]["path"]
                .as_str()
                .or_else(|| result["selector"]["node"]["path"].as_str())
                .unwrap_or(selector)
                .to_owned();
            result["governing_invariants"] = reader::governing_invariants(conn, &selected_id)?;
            let path_is_indexed: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM path_dictionary WHERE path=?1)",
                [&selected_path],
                |row| row.get(0),
            )?;
            result["affected_docs"] = if path_is_indexed {
                paging::query(
                    conn,
                    "SELECT d.doc_id,d.path,d.section_title,d.doc_type,e.line FROM edges e JOIN nodes symbol ON symbol.node_hash=e.src_hash JOIN path_dictionary sp ON sp.path_id=symbol.path_id JOIN nodes dn ON dn.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=dn.id WHERE e.kind='references_doc' AND sp.path=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' UNION SELECT d.doc_id,d.path,d.section_title,d.doc_type,CAST(json_extract(c.value,'$.line') AS INTEGER) FROM nodes dn JOIN doc_sections d ON d.doc_id=dn.id JOIN json_each(dn.details,'$.symbol_claims') c WHERE json_extract(c.value,'$.target_path')=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' ORDER BY 2,1,5",
                    &[&selected_path],
                    request.page,
                )?
            } else {
                paging::query(
                    conn,
                    "SELECT DISTINCT d.doc_id,d.path,d.section_title,d.doc_type,e.line FROM edges e JOIN nodes dn ON dn.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=dn.id WHERE e.kind='references_doc' AND e.src_hash=?1 AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%' ORDER BY d.path,d.doc_id,e.line",
                    &[&stable_hash64(&selected_id)],
                    request.page,
                )?
            };
            Ok(result)
        }
        (None, Some(mode)) => changed_impact_with_path_options(conn, root, mode, path, request),
        (Some(_), Some(_)) => bail!("provide exactly one of selector or changes"),
        (None, None) if empty_selector => traverse_with_path_options(
            conn,
            selector.context("empty selector was unexpectedly absent")?,
            path,
            request,
        ),
        (None, None) => bail!("provide exactly one of selector or changes"),
    }
}

fn sql_selector(selector: Option<&str>) -> Result<&str> {
    selector
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("sql operation requires a SELECT or WITH selector"))
}

/// Traverse direct indexed relationships with bounded paging.
pub fn traverse_with_options(
    conn: &Connection,
    selector: &str,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    traverse_with_path_options(conn, selector, None, request)
}

pub(crate) fn traverse_with_path_options(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    request: &TraversalOptions<'_>,
) -> Result<Value> {
    let depth = request.depth;
    let inbound = request.inbound;
    let mode = request.mode;
    let edge_types = request.edge_types;
    let options = request.page;
    if depth > 16 {
        bail!("depth must be <=16");
    }
    let generation = paging::generation(conn, options)?;
    let node = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    let id = node["id"].as_str().context("selected node has no id")?;
    let id_hash = stable_hash64(id);
    let immediate = admit_traversal(conn, id, depth, inbound)?;
    let (forward, reverse) = resolve_traversal_kinds(mode, edge_types, true);
    let mode_str = mode.unwrap_or("calls");

    if depth == 0 || (depth > 1 && immediate == 0) {
        let items = if options.offset == 0 {
            let mut seed = node.clone();
            let seed_fields = seed.as_object_mut().context("invalid selected node")?;
            seed_fields.insert("distance".into(), json!(0));
            seed_fields.insert("is_seed".into(), json!(true));
            vec![seed]
        } else {
            Vec::new()
        };
        let mut result = json!({
            "selector": node,
            "direction": if inbound { "inbound" } else { "outbound" },
            "depth": depth,
            "mode": mode_str,
            "nodes": paging::value(items, 1, options, &generation)
        });
        if let Some(types) = edge_types {
            result["edge_types"] = json!(types);
        }
        return Ok(result);
    }
    let steps = custom_dependency_steps(inbound, true, &forward, &reverse);
    if depth == 1 {
        let columns = paging::nodes("n", options.detail);
        let base = format!("WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id) SELECT {columns},r.distance FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id");
        let count = reader::rows(
            conn,
            &format!("SELECT count(*) total FROM ({base})"),
            &[&id_hash, &depth],
            1,
        )?;
        let total = count[0]["total"]
            .as_u64()
            .context("traversal total is missing")?;
        let total = usize::try_from(total)?;
        let limit = i64::try_from(options.limit)?;
        let offset = i64::try_from(options.offset)?;
        let (from, to) = if inbound {
            ("dst_hash", "src_hash")
        } else {
            ("src_hash", "dst_hash")
        };
        let reverse_edge = if reverse.is_empty() {
            String::new()
        } else {
            format!(" OR (e.{to}=?1 AND e.{from}=n.node_hash AND e.kind IN({reverse}))")
        };
        let evidence = if options.detail == acdd_core::response::Detail::Full {
            ", 'evidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.evidence_id)"
        } else {
            ""
        };
        let edge_json = format!(
            "CASE WHEN r.distance=1 THEN (SELECT json_object('src_public_id',(SELECT id FROM nodes WHERE node_hash=e.src_hash),'dst_public_id',(SELECT id FROM nodes WHERE node_hash=e.dst_hash),'kind',e.kind,'path',(SELECT path FROM path_dictionary WHERE path_id=e.path_id),'line',e.line,'confidence',(SELECT evidence FROM coverage_evidence WHERE evidence_id=e.confidence_id),'occurrence_count',e.occurrence_count,'direction',CASE WHEN e.src_hash=?1 THEN 'outbound' ELSE 'inbound' END{evidence}) FROM edges e WHERE ((e.{from}=?1 AND e.{to}=n.node_hash AND e.kind IN({forward})){reverse_edge}) ORDER BY e.kind,e.src_hash,e.dst_hash,e.path_id,e.line,e.evidence_id LIMIT 1) END AS __edge"
        );
        let page_sql = format!("WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id) SELECT {columns},r.distance,{edge_json} FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4");
        let mut items = reader::rows(
            conn,
            &page_sql,
            &[&id_hash, &depth, &limit, &offset],
            options.limit,
        )?;
        for item in &mut items {
            let fields = item.as_object_mut().context("invalid traversal node")?;
            let edge = decode_edge_descriptor(fields.remove("__edge").unwrap_or(Value::Null))?;
            fields.insert(
                "is_seed".into(),
                json!(fields.get("id").and_then(Value::as_str) == Some(id)),
            );
            if !edge.is_null() {
                fields.insert("edge".into(), edge);
            }
        }
        let mut result = json!({
            "selector": node,
            "direction": if inbound { "inbound" } else { "outbound" },
            "depth": depth,
            "mode": mode_str,
            "nodes": paging::value(items, total, options, &generation)
        });
        if let Some(types) = edge_types {
            result["edge_types"] = json!(types);
        }
        return Ok(result);
    }
    // Reuse the bounded reachability result for the exact count, page, and path evidence.
    let columns = paging::nodes("n", options.detail);
    let (forward_from, forward_to, reverse_from, reverse_to) = if inbound {
        ("src_hash", "dst_hash", "dst_hash", "src_hash")
    } else {
        ("dst_hash", "src_hash", "src_hash", "dst_hash")
    };
    let reverse_union = if reverse.is_empty() {
        String::new()
    } else {
        format!("UNION ALL SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{reverse_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{reverse_to} WHERE e.{reverse_from}=p.__hash AND e.kind IN({reverse})")
    };
    let sql = if immediate <= 3 && options.offset == 0 {
        // On sparse walks, count the reached rows in the page scan itself.
        format!("WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), \
        reached(id,distance) AS (SELECT id,min(depth) FROM walk GROUP BY id), \
        page AS (SELECT {columns},n.node_hash __hash,r.distance,count(*) OVER() __total FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4) \
        SELECT p.*,CASE WHEN p.distance=0 THEN NULL ELSE (SELECT json_array(predecessor,edge_kind) FROM ( \
            SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{forward_to} WHERE e.{forward_from}=p.__hash AND e.kind IN({forward}) \
            {reverse_union} \
        ) ORDER BY predecessor,edge_kind LIMIT 1) END __reason FROM page p")
    } else {
        format!("WITH RECURSIVE walk(id,depth) AS (SELECT ?1,0 UNION {steps}), \
        reached(id,distance) AS MATERIALIZED (SELECT id,min(depth) FROM walk GROUP BY id), \
        page AS (SELECT {columns},n.node_hash __hash,r.distance FROM reached r JOIN nodes n ON n.node_hash=r.id ORDER BY r.distance,n.id LIMIT ?3 OFFSET ?4) \
        SELECT totals.__total,p.*,CASE WHEN p.distance=0 THEN NULL ELSE (SELECT json_array(predecessor,edge_kind) FROM ( \
            SELECT predecessor.id AS predecessor,e.kind AS edge_kind FROM edges e JOIN reached prior ON prior.id=e.{forward_to} AND prior.distance=p.distance-1 JOIN nodes predecessor ON predecessor.node_hash=e.{forward_to} WHERE e.{forward_from}=p.__hash AND e.kind IN({forward}) \
            {reverse_union} \
        ) ORDER BY predecessor,edge_kind LIMIT 1) END __reason \
        FROM (SELECT count(*) __total FROM reached r JOIN nodes n ON n.node_hash=r.id) totals \
        LEFT JOIN page p ON 1 ORDER BY p.distance,p.id")
    };
    let limit = i64::try_from(options.limit)?;
    let offset = i64::try_from(options.offset)?;
    let mut items = reader::rows(
        conn,
        &sql,
        &[&id_hash, &depth, &limit, &offset],
        options.limit,
    )?;
    let total = items
        .first()
        .and_then(|row| row["__total"].as_u64())
        .context("traversal total is missing")?;
    let total = usize::try_from(total)?;
    items.retain(|row| !row["id"].is_null());
    for item in &mut items {
        let item = item.as_object_mut().context("invalid traversal node")?;
        item.remove("__total");
        item.remove("__hash");
        let reason = item
            .remove("__reason")
            .and_then(|value| value.as_str().map(str::to_owned));
        let reason = reason
            .map(|encoded| serde_json::from_str::<(String, String)>(&encoded))
            .transpose()?;
        let predecessor = item
            .remove("__predecessor")
            .and_then(|value| value.as_str().map(str::to_owned))
            .or_else(|| reason.as_ref().map(|pair| pair.0.clone()));
        let edge_kind = item
            .remove("__edge_kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            .or_else(|| reason.map(|pair| pair.1));
        if item.get("id").and_then(Value::as_str) == Some(id) {
            item.insert("is_seed".into(), json!(true));
        } else {
            item.insert("is_seed".into(), json!(false));
            item.insert("via".into(), json!({"id":predecessor.context("reached node has no predecessor")?,"kind":edge_kind.context("reached node has no edge kind")?}));
        }
    }
    let mut result = json!({
        "selector": node,
        "direction": if inbound { "inbound" } else { "outbound" },
        "depth": depth,
        "mode": mode_str,
        "nodes": paging::value(items, total, options, &generation),
    });
    if let Some(types) = edge_types {
        result["edge_types"] = json!(types);
    }
    Ok(result)
}

fn slice_paged(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    depth: u32,
    options: &QueryOptions,
) -> Result<Value> {
    if depth > 16 {
        bail!("depth must be <=16");
    }
    paging::generation(conn, options)?;
    let directory_selector = selector.trim();
    let directory_selector = directory_selector
        .strip_prefix("file://")
        .or_else(|| directory_selector.strip_prefix("file:"))
        .unwrap_or(directory_selector);
    let directory_selector = directory_selector
        .strip_prefix("./")
        .unwrap_or(directory_selector)
        .trim_end_matches('/');
    match reader::select_detail_with_path(conn, selector, path, options.detail) {
        Ok(node)
            if node["kind"] == "component" && node["path"].as_str() == Some(directory_selector) => {
        }
        Ok(_) => {
            return traverse_with_path_options(
                conn,
                selector,
                path,
                &TraversalOptions {
                    depth,
                    inbound: false,
                    mode: None,
                    edge_types: None,
                    page: options,
                },
            );
        }
        Err(error)
            if !matches!(
                error.downcast_ref::<reader::SelectorError>(),
                Some(reader::SelectorError::NotFound { .. })
            ) =>
        {
            return Err(error);
        }
        Err(_) => {}
    }
    let Some(path) = reader::directory_path(conn, selector)? else {
        let suggestions = reader::indexed_path_suggestions(conn, selector)?;
        return Err(reader::SelectorError::NotFound {
            selector: selector.to_owned(),
            suggestions,
        }
        .into());
    };
    let (lower, upper) = reader::path_bounds(&path);
    let nodes = paging::query(
        conn,
        &format!(
            "SELECT {} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path>=?1 AND p.path<?2 \
             ORDER BY CASE WHEN n.kind IN ('module','file') THEN 0 ELSE 1 END,p.path,n.line,n.id",
            paging::nodes_with_path("n", options.detail, "p.path")
        ),
        &[&lower, &upper],
        options,
    )?;
    let (boundary, edges) = directory_boundary(conn, &lower, &upper, options)?;
    Ok(json!({
        "selector": {"kind":"directory","path":path},
        "scope":"directory_members",
        "direction":"outbound",
        "requested_depth":depth,
        "graph_depth_applied":0,
        "nodes":nodes,
        "edges":edges,
        "boundary_contract":boundary,
        "hint":"Directory mode lists indexed nodes with graph depth 0. Select a returned exact node id or file path to traverse graph relationships at the requested depth."
    }))
}

// Paths are stored once in the dictionary. Starting at the indexed path range
// keeps the query local to directory nodes and their incident indexed edges.
fn boundary_incidence(inbound: bool) -> String {
    let (inside_key, outside_key) = if inbound {
        ("dst_hash", "src_hash")
    } else {
        ("src_hash", "dst_hash")
    };
    format!(
        "SELECT inside.id AS inside_id,inside.name AS inside_name,
                json_extract(inside.details,'$.signature') AS inside_signature,
                outside.id AS outside_id,outside.name AS outside_name,
                json_extract(outside.details,'$.signature') AS outside_signature,
                outside.path_id AS outside_path_id,e.kind AS kind
         FROM path_dictionary p
         CROSS JOIN nodes inside ON inside.path_id=p.path_id
         CROSS JOIN edges e ON e.{inside_key}=inside.node_hash
         CROSS JOIN nodes outside ON outside.node_hash=e.{outside_key}
         CROSS JOIN path_dictionary op ON op.path_id=outside.path_id
         WHERE p.path>=?1 AND p.path<?2 AND (op.path<?1 OR op.path>=?2)"
    )
}

fn boundary_modules(
    conn: &Connection,
    incidence: &str,
    inbound: bool,
    lower: &str,
    upper: &str,
    options: &QueryOptions,
) -> Result<Value> {
    let (field, example) = if inbound {
        (
            "symbol_pairs",
            "json_object('source_id',outside_id,'target_id',inside_id,'kind',kind)",
        )
    } else {
        ("targets", "json_object('target_id',outside_id,'name',outside_name,'signature',outside_signature,'source_id',inside_id,'kind',kind)")
    };
    // Rank representatives in the same stable order as the old boundary
    // contract. Only the requested module page is materialized in Rust.
    let sql = format!(
        "WITH RECURSIVE boundary AS MATERIALIZED ({incidence}),
         path_parts(path_id,remaining,prefix) AS (
             SELECT DISTINCT outside_path_id,op.path,'' FROM boundary
             JOIN path_dictionary op ON op.path_id=outside_path_id
             UNION ALL
             SELECT path_id,substr(remaining,instr(remaining,'/')+1),
                    prefix || substr(remaining,1,instr(remaining,'/'))
             FROM path_parts WHERE instr(remaining,'/')>0
         ), path_modules AS (
             SELECT path_id,CASE WHEN prefix='' THEN ''
                    ELSE substr(prefix,1,length(prefix)-1) END AS module
             FROM path_parts WHERE instr(remaining,'/')=0
         ),
         ranked AS (
             SELECT boundary.*,path_modules.module,row_number() OVER (
                 PARTITION BY path_modules.module ORDER BY outside_id,inside_id,kind
             ) AS example_rank FROM boundary JOIN path_modules
               ON path_modules.path_id=boundary.outside_path_id
         )
         SELECT module,count(*) AS edge_count,
                json_group_array({example} ORDER BY example_rank)
                    FILTER (WHERE example_rank<=5) AS {field}
         FROM ranked GROUP BY module ORDER BY module"
    );
    let mut page = paging::query(conn, &sql, &[&lower, &upper], options)?;
    if let Some(items) = page.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            let text = item[field].as_str().context("invalid boundary examples")?;
            item[field] = serde_json::from_str(text)?;
        }
    }
    Ok(page)
}

fn directory_boundary(
    conn: &Connection,
    lower: &str,
    upper: &str,
    options: &QueryOptions,
) -> Result<(Value, Value)> {
    let incoming = boundary_incidence(true);
    let outgoing = boundary_incidence(false);
    let exports = paging::query(
        conn,
        &format!(
            "SELECT inside_id AS id,inside_name AS name,inside_signature AS signature,
                    count(DISTINCT outside_id) AS external_caller_count
             FROM ({incoming}) GROUP BY inside_id ORDER BY inside_id"
        ),
        &[&lower, &upper],
        options,
    )?;
    let inbound = boundary_modules(conn, &incoming, true, lower, upper, options)?;
    let outbound = boundary_modules(conn, &outgoing, false, lower, upper, options)?;
    // Match the original bytewise ordering of the insertion-ordered JSON object.
    let evidence = if options.detail == acdd_core::response::Detail::Full {
        ",'evidence',evidence"
    } else {
        ""
    };
    let internal = paging::query(
        conn,
        &format!(
            "SELECT * FROM (SELECT {} FROM path_dictionary p
             CROSS JOIN nodes inside ON inside.path_id=p.path_id
             CROSS JOIN edges e ON e.src_hash=inside.node_hash
             CROSS JOIN nodes outside ON outside.node_hash=e.dst_hash
             CROSS JOIN path_dictionary op ON op.path_id=outside.path_id
             WHERE p.path>=?1 AND p.path<?2 AND op.path>=?1 AND op.path<?2)
             ORDER BY json_object('src_public_id',src_public_id,'dst_public_id',dst_public_id,
                'kind',kind,'line',line,'occurrence_count',occurrence_count,
                'path',path,'confidence',confidence {evidence})",
            paging::edges("e", options.detail)
        ),
        &[&lower, &upper],
        options,
    )?;
    Ok((
        json!({"exported_symbols":exports,"inbound_modules":inbound,"outbound_modules":outbound}),
        internal,
    ))
}

/// Performs removal paged.
pub fn removal_paged(conn: &Connection, selector: &str, options: &QueryOptions) -> Result<Value> {
    removal_paged_with_path(conn, selector, None, options)
}

/// Returns removal proof or blocked dependents for a symbol selector.
pub fn removal_paged_with_path(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let trimmed = selector.trim();
    let unpeeled = trimmed
        .strip_prefix("file://")
        .or_else(|| trimmed.strip_prefix("file:"))
        .unwrap_or(trimmed);
    let normalized_path = unpeeled.strip_prefix("./").unwrap_or(unpeeled);
    if normalized_path.ends_with(".md") {
        return Err(reader::SelectorError::DocLink {
            target: normalized_path.to_owned(),
        }
        .into());
    }
    let generation = paging::generation(conn, options)?;
    let selected_node = reader::select_detail_with_path(
        conn,
        selector,
        path,
        acdd_core::response::Detail::Compact,
    )?;
    let file_count = paging::count(
        conn,
        "SELECT count(*) FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1",
        &[&normalized_path],
    )?;
    let (selection, selected_id) = if file_count > 0 {
        (
            "SELECT n.node_hash FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1",
            normalized_path.to_owned(),
        )
    } else {
        (
            "SELECT node_hash FROM nodes WHERE id=?1",
            selected_node["id"].as_str().unwrap_or("").to_owned(),
        )
    };
    if file_count > 10000 {
        bail!("removal scope exceeds 10000 nodes");
    }
    let selected = format!("selected(hash) AS ({selection})");
    let dependency_rows = format!("SELECT e.src_hash,e.dst_hash,e.kind FROM edges e WHERE e.dst_hash IN(SELECT hash FROM selected) AND e.src_hash NOT IN(SELECT hash FROM selected) AND e.kind NOT IN('contains','references_doc',{REVERSE_DEPENDENCIES}) UNION SELECT e.src_hash,e.dst_hash,e.kind FROM edges e WHERE e.src_hash IN(SELECT hash FROM selected) AND e.dst_hash NOT IN(SELECT hash FROM selected) AND e.kind IN({REVERSE_DEPENDENCIES})");
    let dependency_cte =
        format!("WITH {selected}, dependencies AS MATERIALIZED ({dependency_rows})");
    let selection_sql = if file_count > 0 {
        "SELECT n.id FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?1 ORDER BY n.id"
    } else {
        "SELECT id FROM nodes WHERE id=?1 ORDER BY id"
    };
    let diagnostic_selected = reader::rows(conn, selection_sql, &[&selected_id], 10001)?;
    let selected_ids: Vec<String> = diagnostic_selected
        .iter()
        .filter_map(|node| node["id"].as_str().map(str::to_owned))
        .collect();
    let (target_unresolved, target_errors) = removal_diagnostics(conn, &selected_ids)?;
    let selected_total = selected_ids.len();
    let selected_items = selected_ids
        .iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|id| json!({"id":id}))
        .collect();
    let selected_page = paging::value(selected_items, selected_total, options, &generation);
    let callers = paging::query(
        conn,
        &format!("{dependency_cte} SELECT {} FROM dependencies d JOIN edges e ON e.src_hash=d.src_hash AND e.dst_hash=d.dst_hash AND e.kind=d.kind ORDER BY e.path_id,e.line,e.src_hash,e.dst_hash,e.kind", paging::edges("e", options.detail)),
        &[&selected_id],
        options,
    )?;
    let dependency_count = callers["total"]
        .as_u64()
        .map(usize::try_from)
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("removal dependency total is missing"))?;
    let safe_to_remove = dependency_count == 0 && target_unresolved == 0 && target_errors == 0;
    Ok(
        json!({"assessment":removal_assessment(dependency_count,true,target_unresolved,target_errors),"selector":selector,
        "selected_ids":selected_page,
        "incoming_dependencies":callers,"safe_to_remove":safe_to_remove,"unresolved_references":target_unresolved,"parse_errors":target_errors,
        "proof_scope":"indexed static references only; dynamic entrypoints and external callers require separate authority",
        "generation":generation}),
    )
}

/// Execute a bounded graph operation through one options contract.
pub fn query_with_options(
    conn: &Connection,
    operation: &str,
    selector: Option<&str>,
    request: &GraphQueryOptions<'_>,
) -> Result<Value> {
    query_with_path_options(conn, operation, selector, None, request)
}

fn compute_line_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut matching = 0;
    let min_len = a.len().min(b.len());
    for i in 0..min_len {
        if a[i] == b[i] {
            matching += 1;
        }
    }
    let max_len = a.len().max(b.len());
    matching as f64 / max_len as f64
}

fn format_suggestion(name: &str, locations: &[Value]) -> String {
    let has_build = locations
        .iter()
        .any(|l| l["path"].as_str().is_some_and(|p| p.ends_with("build.rs")));
    if has_build {
        return format!(
            "Consider extracting '{name}' into a shared build module (e.g. crates/acdd-index/build_manifests.rs) and referencing it from build scripts."
        );
    }
    let has_acdd_core = locations.iter().any(|l| {
        l["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("crates/acdd-core"))
    });
    if has_acdd_core {
        return format!(
            "Consider centralizing '{name}' in 'crates/acdd-core' and re-exporting it in dependent crates."
        );
    }
    let mut unique_crates = std::collections::HashSet::new();
    for l in locations {
        if let Some(p) = l["path"].as_str() {
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

fn normalize_symbol_body_lines(lines: &[String]) -> (Option<String>, Option<Vec<String>>) {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    let (Some(start), Some(end)) = (start, end) else {
        return (None, None);
    };
    if start > end {
        return (None, None);
    }
    let slice = &lines[start..=end];

    // Exact hash preserves unmodified slice content to guarantee literal preservation
    let mut exact_hasher = Sha256::new();
    for line in slice {
        exact_hasher.update(line.as_bytes());
        exact_hasher.update(b"\n");
    }
    let hash_hex = format!("{:x}", exact_hasher.finalize());

    // Normalized lines are retained for near-clone similarity clustering
    let min_indent = slice
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);

    let common_indent: String = if min_indent > 0 {
        slice
            .iter()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.chars().take(min_indent).collect())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let mut normalized = Vec::with_capacity(slice.len());
    for line in slice {
        let stripped = if !common_indent.is_empty()
            && !line.trim().is_empty()
            && line.starts_with(&common_indent)
        {
            &line[common_indent.len()..]
        } else {
            line.as_str()
        };
        normalized.push(stripped.to_string());
    }

    (Some(hash_hex), Some(normalized))
}

fn normalize_scope(target: Option<&str>, path: Option<&str>, label: &str) -> Result<String> {
    if let (Some(target), Some(path)) = (target, path) {
        anyhow::ensure!(target == path, "{label} conflicts with path");
    }
    let raw = target.or(path).unwrap_or("").trim();
    let raw = raw
        .strip_prefix("file://")
        .or_else(|| raw.strip_prefix("file:"))
        .unwrap_or(raw);
    let raw = raw.strip_prefix("./").unwrap_or(raw).trim_end_matches('/');
    let scope = if raw == "." { "" } else { raw };
    anyhow::ensure!(
        !scope.starts_with('/')
            && !scope
                .split('/')
                .any(|s| s == ".." || s == "." || (s.is_empty() && !scope.is_empty())),
        "{label} must be workspace-relative"
    );
    Ok(scope.to_owned())
}

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

    let min_lines = min_lines.unwrap_or(10).max(1);

    if level_str == "file" {
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
            let entry = groups.entry(digest).or_insert((size, Vec::new()));
            entry.1.push(p);
        }
        drop(rows);
        drop(stmt);

        let mut items = Vec::new();
        let mut total_duplicated_bytes: u64 = 0;
        for (digest, (size, mut locations)) in groups {
            if locations.len() < 2 {
                continue;
            }
            locations.sort();
            let duplicate_count = locations.len();
            total_duplicated_bytes += size * (duplicate_count as u64 - 1);
            let suggestion = format!(
                "Consolidate duplicate files across {} locations into a single shared file or replace with re-exports.",
                locations.len()
            );
            items.push(json!({
                "digest": digest,
                "size_bytes": size,
                "duplicate_count": duplicate_count,
                "locations": locations,
                "suggestion": suggestion,
            }));
        }
        items.sort_by(|a, b| {
            let size_a = a["size_bytes"].as_u64().unwrap_or(0);
            let size_b = b["size_bytes"].as_u64().unwrap_or(0);
            size_b
                .cmp(&size_a)
                .then_with(|| {
                    let count_a = a["duplicate_count"].as_u64().unwrap_or(0);
                    let count_b = b["duplicate_count"].as_u64().unwrap_or(0);
                    count_b.cmp(&count_a)
                })
                .then_with(|| {
                    let dig_a = a["digest"].as_str().unwrap_or("");
                    let dig_b = b["digest"].as_str().unwrap_or("");
                    dig_a.cmp(dig_b)
                })
        });
        let total = items.len();
        let paged: Vec<Value> = items
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .collect();
        let next = options.offset.saturating_add(paged.len());
        let has_more = next < total;

        return Ok(json!({
            "operation": "duplicates",
            "level": "file",
            "scope": if scope.is_empty() { None } else { Some(scope) },
            "total_duplicate_groups": total,
            "total_duplicated_bytes": total_duplicated_bytes,
            "items": paged,
            "total": total,
            "offset": options.offset,
            "limit": options.limit,
            "has_more": has_more,
            "next_offset": if has_more { Some(next) } else { None },
            "generation": generation,
        }));
    }

    let (lower, upper) = reader::path_bounds(scope);
    let workspace_root: Option<std::path::PathBuf> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'workspace_root'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(std::path::PathBuf::from);

    struct Candidate {
        name: String,
        kind: String,
        line: usize,
        end_line: usize,
        path: String,
        body_hash: Option<String>,
        normalized_lines: Option<Vec<String>>,
    }

    let mut stmt = conn.prepare(
        "SELECT n.name, n.kind, n.line, n.end_line, p.path
         FROM nodes n
         JOIN path_dictionary p ON p.path_id = n.path_id
         WHERE n.kind IN ('function', 'method')
           AND (n.end_line - n.line + 1) >= ?1
           AND (?2 = '' OR p.path = ?2 OR (p.path >= ?3 AND p.path < ?4))
         ORDER BY p.path, n.line",
    )?;

    let mut file_cache: std::collections::HashMap<String, Option<Vec<String>>> =
        std::collections::HashMap::new();
    let mut source_loader = super::symbols::VerifiedSourceCache::new();
    let mut stale_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let mut rows = stmt.query(rusqlite::params![min_lines, scope, lower, upper])?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let line: usize = row.get(2)?;
        let end_line: usize = row.get(3)?;
        let path: String = row.get(4)?;

        let mut body_hash = None;
        let mut normalized_lines = None;

        if let Some(root) = &workspace_root {
            let lines = file_cache.entry(path.clone()).or_insert_with(|| {
                match source_loader.read(conn, root, &path) {
                    Ok(content) => Some(content.lines().map(|s| s.to_string()).collect()),
                    Err(_) => {
                        stale_files.insert(path.clone());
                        None
                    }
                }
            });
            if lines.is_none() {
                stale_files.insert(path.clone());
            }

            if let Some(lines) = lines {
                if line > 0 && end_line >= line && line <= lines.len() {
                    let end_idx = end_line.min(lines.len());
                    let slice = &lines[(line - 1)..end_idx];
                    let (h, norm) = normalize_symbol_body_lines(slice);
                    body_hash = h;
                    normalized_lines = norm;
                }
            }
        }

        candidates.push(Candidate {
            name,
            kind,
            line,
            end_line,
            path,
            body_hash,
            normalized_lines,
        });
    }
    drop(rows);
    drop(stmt);

    let mut exact_groups: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        if let Some(hash) = &c.body_hash {
            exact_groups.entry(hash.clone()).or_default().push(idx);
        }
    }

    struct DuplicateGroup {
        name: String,
        kind: String,
        line_count: usize,
        match_type: String,
        locations: Vec<Value>,
        duplicate_count: usize,
        duplicated_lines: usize,
        suggestion: String,
    }

    let mut result_groups = Vec::new();
    let mut counted_duplicate_locations: std::collections::HashSet<(String, usize)> =
        std::collections::HashSet::new();

    for (_, indices) in exact_groups {
        if indices.len() < 2 {
            continue;
        }
        let mut unique_locs = Vec::new();
        let mut seen_locs = std::collections::HashSet::new();
        for &idx in &indices {
            let c = &candidates[idx];
            if seen_locs.insert((c.path.clone(), c.line)) {
                unique_locs.push(idx);
            }
        }
        if unique_locs.len() < 2 {
            continue;
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
        let locations: Vec<Value> = unique_locs
            .iter()
            .map(|&i| {
                let c = &candidates[i];
                json!({
                    "path": c.path,
                    "line": c.line,
                    "end_line": c.end_line,
                })
            })
            .collect();
        let dup_count = locations.len();
        let duplicated_lines = line_count * dup_line_locations;
        let suggestion = format_suggestion(&first.name, &locations);
        result_groups.push(DuplicateGroup {
            name: first.name.clone(),
            kind: first.kind.clone(),
            line_count,
            match_type: "exact".to_string(),
            locations,
            duplicate_count: dup_count,
            duplicated_lines,
            suggestion,
        });
    }

    let mut by_name: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (idx, c) in candidates.iter().enumerate() {
        if c.normalized_lines.is_some() {
            by_name.entry(c.name.clone()).or_default().push(idx);
        }
    }

    for (name, indices) in by_name {
        if indices.len() < 2 {
            continue;
        }
        let mut hash_to_candidates: std::collections::BTreeMap<String, Vec<usize>> =
            std::collections::BTreeMap::new();
        for &i in &indices {
            if let Some(h) = &candidates[i].body_hash {
                hash_to_candidates.entry(h.clone()).or_default().push(i);
            }
        }
        if hash_to_candidates.len() < 2 {
            continue;
        }

        let distinct_hashes: Vec<(String, usize)> = hash_to_candidates
            .iter()
            .map(|(h, inds)| (h.clone(), inds[0]))
            .collect();

        let mut hash_clusters: Vec<Vec<String>> = Vec::new();
        for (h, rep_idx) in distinct_hashes {
            let lines_rep = candidates[rep_idx].normalized_lines.as_ref().unwrap();
            let mut clustered = false;
            for cluster in &mut hash_clusters {
                let rep_h = &cluster[0];
                let cluster_rep_idx = hash_to_candidates[rep_h][0];
                let cluster_rep_lines = candidates[cluster_rep_idx]
                    .normalized_lines
                    .as_ref()
                    .unwrap();
                let sim = compute_line_similarity(lines_rep, cluster_rep_lines);
                if sim >= 0.80 {
                    cluster.push(h.clone());
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
            let mut all_indices = Vec::new();
            let mut seen_locs = std::collections::HashSet::new();
            for h in &cluster {
                for &idx in &hash_to_candidates[h] {
                    let c = &candidates[idx];
                    if seen_locs.insert((c.path.clone(), c.line)) {
                        all_indices.push(idx);
                    }
                }
            }
            if all_indices.len() < 2 {
                continue;
            }

            all_indices.sort_by(|&a, &b| {
                candidates[a]
                    .path
                    .cmp(&candidates[b].path)
                    .then_with(|| candidates[a].line.cmp(&candidates[b].line))
            });

            let mut dup_line_locations = 0;
            for &idx in &all_indices[1..] {
                let c = &candidates[idx];
                if counted_duplicate_locations.insert((c.path.clone(), c.line)) {
                    dup_line_locations += 1;
                }
            }

            let first = &candidates[all_indices[0]];
            let line_count = first.end_line.saturating_sub(first.line) + 1;
            let locations: Vec<Value> = all_indices
                .iter()
                .map(|&i| {
                    let c = &candidates[i];
                    json!({
                        "path": c.path,
                        "line": c.line,
                        "end_line": c.end_line,
                    })
                })
                .collect();
            let dup_count = locations.len();
            let duplicated_lines = line_count * dup_line_locations;
            let suggestion = format_suggestion(&name, &locations);
            result_groups.push(DuplicateGroup {
                name: name.clone(),
                kind: first.kind.clone(),
                line_count,
                match_type: "similar".to_string(),
                locations,
                duplicate_count: dup_count,
                duplicated_lines,
                suggestion,
            });
        }
    }

    result_groups.sort_by(|a, b| {
        b.duplicated_lines
            .cmp(&a.duplicated_lines)
            .then_with(|| b.duplicate_count.cmp(&a.duplicate_count))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.match_type.cmp(&b.match_type))
            .then_with(|| {
                let path_a = a
                    .locations
                    .first()
                    .and_then(|l| l.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                let path_b = b
                    .locations
                    .first()
                    .and_then(|l| l.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                path_a.cmp(path_b)
            })
            .then_with(|| {
                let line_a = a
                    .locations
                    .first()
                    .and_then(|l| l.get("line"))
                    .and_then(|p| p.as_u64())
                    .unwrap_or(0);
                let line_b = b
                    .locations
                    .first()
                    .and_then(|l| l.get("line"))
                    .and_then(|p| p.as_u64())
                    .unwrap_or(0);
                line_a.cmp(&line_b)
            })
    });

    let total = result_groups.len();
    let total_duplicated_lines: usize = result_groups.iter().map(|g| g.duplicated_lines).sum();

    let items: Vec<Value> = result_groups
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|g| {
            json!({
                "name": g.name,
                "kind": g.kind,
                "line_count": g.line_count,
                "match_type": g.match_type,
                "locations": g.locations,
                "duplicate_count": g.duplicate_count,
                "duplicated_lines": g.duplicated_lines,
                "suggestion": g.suggestion,
            })
        })
        .collect();

    let next = options.offset.saturating_add(items.len());
    let has_more = next < total;

    let mut response = json!({
        "operation": "duplicates",
        "level": "symbol",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "min_lines": min_lines,
        "total_duplicate_groups": total,
        "total_duplicated_lines": total_duplicated_lines,
        "items": items,
        "total": total,
        "offset": options.offset,
        "limit": options.limit,
        "has_more": has_more,
        "next_offset": if has_more { Some(next) } else { None },
        "generation": generation,
    });

    if !stale_files.is_empty() {
        response["stale_files"] = json!(stale_files);
    }

    Ok(response)
}

fn untested_paged(
    conn: &Connection,
    selector: Option<&str>,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let scope = normalize_scope(selector, path, "untested selector")?;
    let (lower, upper) = reader::path_bounds(&scope);
    let nodes = paging::query(conn, &format!("SELECT {},json_extract(n.details,'$.signature') AS signature FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.kind IN('function','method') AND n.is_test=0 AND (?1='' OR p.path=?1 OR (p.path>=?2 AND p.path<?3)) AND NOT EXISTS (SELECT 1 FROM edges e JOIN nodes caller ON caller.node_hash=e.src_hash WHERE e.dst_hash=n.node_hash AND e.kind='calls' AND caller.is_test=1) ORDER BY p.path,n.line,n.id", paging::nodes_with_path("n", options.detail, "p.path")), &[&scope, &lower, &upper], options)?;
    Ok(
        json!({"nodes":nodes,"meaning":"no direct indexed test caller; not a runtime coverage proof"}),
    )
}

struct DeadTailsNode {
    hash: i64,
    id: String,
    kind: String,
    name: String,
    qualname: String,
    path: String,
    line: i64,
    end_line: i64,
    is_test_ctx: bool,
    generated: bool,
    has_entrypoint: bool,
    has_impl_trait: bool,
    has_trait_default: bool,
    has_tool_decorator: bool,
}

struct DeadTailsGraph {
    nodes: HashMap<i64, DeadTailsNode>,
    fwd: HashMap<i64, Vec<i64>>,
    candidates: HashSet<i64>,
    roots: HashSet<i64>,
    node_names: HashSet<String>,
    module_names: HashSet<String>,
}

fn is_test_context_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("/tests.rs")
        || path == "tests.rs"
        || path.starts_with("benchmarks/")
}

fn is_under_cfg_test_module(qualname: &str, cfg_test_mods: &HashSet<String>) -> bool {
    if cfg_test_mods.is_empty() {
        return false;
    }
    if cfg_test_mods.contains(qualname) {
        return true;
    }
    for (idx, ch) in qualname.char_indices() {
        if ch == '.' && cfg_test_mods.contains(&qualname[..idx]) {
            return true;
        }
    }
    false
}

fn parse_node_detail_flags(details_raw: &str) -> (bool, bool, bool, bool, bool) {
    if !details_raw.starts_with('{')
        || !(details_raw.contains("\"entrypoint\"")
            || details_raw.contains("\"impl_trait\"")
            || details_raw.contains("\"trait_default\"")
            || details_raw.contains("\"decorators\""))
    {
        return (false, false, false, false, false);
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(details_raw) else {
        return (false, false, false, false, false);
    };
    let has_entrypoint = map.get("entrypoint").is_some_and(|v| !v.is_null());
    let has_impl_trait = map.get("impl_trait").is_some_and(|v| !v.is_null());
    let has_trait_default = map.get("trait_default").is_some_and(|v| !v.is_null());
    let mut has_cfg_test = false;
    let mut has_tool = false;
    if let Some(decs) = map.get("decorators").and_then(Value::as_array) {
        for dec in decs.iter().filter_map(Value::as_str) {
            if dec.contains("cfg(test)") {
                has_cfg_test = true;
            }
            if dec.starts_with("#[tool") || dec.starts_with("#[rmcp::tool") {
                has_tool = true;
            }
        }
    }
    (
        has_entrypoint,
        has_impl_trait,
        has_trait_default,
        has_cfg_test,
        has_tool,
    )
}

fn load_dead_tails_graph(conn: &Connection) -> Result<DeadTailsGraph> {
    let mut nodes: HashMap<i64, DeadTailsNode> = HashMap::new();
    let mut cfg_test_mods: HashSet<String> = HashSet::new();
    let mut node_names: HashSet<String> = HashSet::new();
    let mut module_names: HashSet<String> = HashSet::new();

    {
        let mut stmt = conn.prepare(
            "SELECT n.node_hash, n.id, n.kind, n.name, n.qualname, p.path, n.line, n.end_line,
                    (n.is_test != 0 OR COALESCE(f.is_test, 0) != 0), n.generated != 0, n.details
             FROM nodes n
             JOIN path_dictionary p ON p.path_id = n.path_id
             LEFT JOIN files f ON f.path = p.path",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let hash: i64 = row.get(0)?;
            let id: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let name: String = row.get(3)?;
            let qualname: String = row.get(4)?;
            let path: String = row.get(5)?;
            let line: i64 = row.get(6)?;
            let end_line: i64 = row.get(7)?;
            let base_test: bool = row.get(8)?;
            let generated: bool = row.get(9)?;
            let details_raw: String = row.get(10)?;

            let (has_entrypoint, has_impl_trait, has_trait_default, has_cfg_test, has_tool) =
                parse_node_detail_flags(&details_raw);

            if kind == "module_declaration" && has_cfg_test {
                cfg_test_mods.insert(qualname.clone());
            }
            if !matches!(
                kind.as_str(),
                "document" | "invariant" | "field" | "variable" | "table" | "component"
            ) {
                node_names.insert(name.clone());
            }
            if matches!(
                kind.as_str(),
                "module" | "module_declaration" | "namespace"
            ) {
                if let Some(tail) = qualname.rsplit('.').next() {
                    if !tail.is_empty() {
                        module_names.insert(tail.to_owned());
                    }
                }
            }

            let is_test_ctx = base_test
                || has_cfg_test
                || is_test_context_path(&path)
                || qualname.contains(".tests.")
                || qualname.ends_with(".tests");

            nodes.insert(
                hash,
                DeadTailsNode {
                    hash,
                    id,
                    kind,
                    name,
                    qualname,
                    path,
                    line,
                    end_line,
                    is_test_ctx,
                    generated,
                    has_entrypoint,
                    has_impl_trait,
                    has_trait_default,
                    has_tool_decorator: has_tool,
                },
            );
        }
    }

    if !cfg_test_mods.is_empty() {
        for node in nodes.values_mut() {
            if !node.is_test_ctx && is_under_cfg_test_module(&node.qualname, &cfg_test_mods) {
                node.is_test_ctx = true;
            }
        }
    }

    let mut fwd_set: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut contains_children: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut trait_members: HashSet<i64> = HashSet::new();
    let mut implements_connected: HashSet<i64> = HashSet::new();
    let mut non_test_ref_targets: HashSet<i64> = HashSet::new();
    let mut non_callable_prod_targets: HashSet<i64> = HashSet::new();
    let mut test_targeted: HashSet<i64> = HashSet::new();

    {
        let mut stmt = conn.prepare(
            "SELECT src_hash, kind, dst_hash
             FROM edges
             WHERE kind IN ('calls', 'references', 'possible_calls', 'implements', 'contains')",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let src: i64 = row.get(0)?;
            let kind: String = row.get(1)?;
            let dst: i64 = row.get(2)?;
            match kind.as_str() {
                "contains" => {
                    if let Some(parent) = nodes.get(&src) {
                        if parent.kind == "trait"
                            || (parent.kind == "impl" && parent.has_impl_trait)
                        {
                            trait_members.insert(dst);
                        }
                    }
                    if src != dst && nodes.contains_key(&src) && nodes.contains_key(&dst) {
                        contains_children.entry(src).or_default().push(dst);
                    }
                }
                "implements" => {
                    implements_connected.insert(src);
                    implements_connected.insert(dst);
                }
                "calls" | "references" | "possible_calls" if src != dst => {
                    let Some(src_node) = nodes.get(&src) else {
                        continue;
                    };
                    if !nodes.contains_key(&dst) {
                        continue;
                    }
                    if src_node.is_test_ctx {
                        test_targeted.insert(dst);
                    }
                    if kind == "references" && !src_node.is_test_ctx {
                        non_test_ref_targets.insert(dst);
                    }
                    if !src_node.is_test_ctx
                        && (!matches!(src_node.kind.as_str(), "function" | "method")
                            || src_node.generated)
                    {
                        non_callable_prod_targets.insert(dst);
                    }
                    fwd_set.entry(src).or_default().insert(dst);
                }
                _ => {}
            }
        }
    }

    let mut candidates = HashSet::new();
    let mut roots = HashSet::new();
    for node in nodes.values() {
        if !matches!(node.kind.as_str(), "function" | "method")
            || node.generated
            || node.is_test_ctx
        {
            continue;
        }
        candidates.insert(node.hash);
        let is_main =
            node.name == "main" && (node.path == "main.rs" || node.path.ends_with("/main.rs"));
        if node.has_entrypoint
            || is_main
            || node.has_tool_decorator
            || implements_connected.contains(&node.hash)
            || trait_members.contains(&node.hash)
            || node.has_trait_default
            || non_test_ref_targets.contains(&node.hash)
            || non_callable_prod_targets.contains(&node.hash)
        {
            roots.insert(node.hash);
        }
    }

    propagate_nested_contains(&nodes, &contains_children, &candidates, &mut fwd_set);
    wire_same_file_callbacks(
        conn,
        &nodes,
        &candidates,
        &test_targeted,
        &mut fwd_set,
        &mut roots,
    )?;

    let fwd = fwd_set
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();

    Ok(DeadTailsGraph {
        nodes,
        fwd,
        candidates,
        roots,
        node_names,
        module_names,
    })
}

fn propagate_nested_contains(
    nodes: &HashMap<i64, DeadTailsNode>,
    contains_children: &HashMap<i64, Vec<i64>>,
    candidates: &HashSet<i64>,
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
) {
    for &outer in candidates {
        let Some(children) = contains_children.get(&outer) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut queue: VecDeque<i64> = children.iter().copied().collect();
        while let Some(child_hash) = queue.pop_front() {
            if !seen.insert(child_hash) {
                continue;
            }
            if candidates.contains(&child_hash) {
                fwd_set.entry(outer).or_default().insert(child_hash);
                continue;
            }
            let Some(child) = nodes.get(&child_hash) else {
                continue;
            };
            if !child.is_test_ctx
                && matches!(child.kind.as_str(), "struct" | "impl" | "enum")
            {
                if let Some(nested) = contains_children.get(&child_hash) {
                    queue.extend(nested.iter().copied());
                }
            }
        }
    }
}

fn strip_comments_and_strings_preserving_lines(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut block_depth = 0usize;

    while i < len {
        if block_depth > 0 {
            if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                out[i] = b' ';
                out[i + 1] = b' ';
                block_depth += 1;
                i += 2;
                continue;
            }
            if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                out[i] = b' ';
                out[i + 1] = b' ';
                block_depth -= 1;
                i += 2;
                continue;
            }
            if bytes[i] != b'\n' {
                out[i] = b' ';
            }
            i += 1;
            continue;
        }

        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i < len && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out[i] = b' ';
            out[i + 1] = b' ';
            block_depth = 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'r'
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        {
            let mut j = i + 1;
            while j < len && bytes[j] == b'#' {
                j += 1;
            }
            if j < len && bytes[j] == b'"' {
                let hashes = j - (i + 1);
                for b in &mut out[i..=j] {
                    *b = b' ';
                }
                i = j + 1;
                while i < len {
                    if bytes[i] == b'"'
                        && i + hashes < len
                        && bytes[i + 1..=i + hashes].iter().all(|&b| b == b'#')
                    {
                        for b in &mut out[i..=i + hashes] {
                            *b = b' ';
                        }
                        i += hashes + 1;
                        break;
                    }
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
                continue;
            }
        }
        if bytes[i] == b'"' {
            out[i] = b' ';
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    if bytes[i + 1] != b'\n' {
                        out[i + 1] = b' ';
                    }
                    i += 2;
                    continue;
                }
                let close = bytes[i] == b'"';
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
                if close {
                    break;
                }
            }
            continue;
        }
        if bytes[i] == b'\'' {
            if i + 2 < len && bytes[i + 1] != b'\\' && bytes[i + 1] != b'\n' && bytes[i + 2] == b'\'' {
                out[i] = b' ';
                out[i + 1] = b' ';
                out[i + 2] = b' ';
                i += 3;
                continue;
            }
            if i + 1 < len && bytes[i + 1] == b'\\' {
                out[i] = b' ';
                i += 1;
                while i < len && bytes[i] != b'\n' {
                    let close = bytes[i] == b'\'';
                    out[i] = b' ';
                    i += 1;
                    if close {
                        break;
                    }
                }
                continue;
            }
        }
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn scan_bare_callback_tokens<F>(
    text: &str,
    match_token: &mut F,
) where
    F: FnMut(&str, bool),
{
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if !(b.is_ascii_alphabetic() || b == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let ident = &text[start..i];
        let prev_non_ws = bytes[..start]
            .iter()
            .rposition(|c| !c.is_ascii_whitespace())
            .map(|idx| (idx, bytes[idx]));
        let next_non_ws = bytes[i..]
            .iter()
            .position(|c| !c.is_ascii_whitespace())
            .map(|rel| bytes[i + rel]);

        let preceded_by_dot = matches!(prev_non_ws, Some((_, b'.')));
        let preceded_by_decl_kw = if let Some((idx, _)) = prev_non_ws {
            let word_end = idx + 1;
            let mut word_start = word_end;
            while word_start > 0
                && (bytes[word_start - 1].is_ascii_alphanumeric()
                    || bytes[word_start - 1] == b'_')
            {
                word_start -= 1;
            }
            matches!(
                &text[word_start..word_end],
                "fn" | "mod"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "type"
                    | "let"
                    | "mut"
                    | "const"
                    | "static"
                    | "for"
            )
        } else {
            false
        };
        let preceded_by_scope = matches!(
            prev_non_ws,
            Some((idx, b':')) if idx > 0 && bytes[idx - 1] == b':'
        );
        let followed_by_call_or_colon = matches!(next_non_ws, Some(b'(' | b':'));

        if !preceded_by_dot && !preceded_by_decl_kw && !followed_by_call_or_colon {
            match_token(ident, preceded_by_scope);
        }
    }
}

fn starts_top_level_const_or_static(trimmed: &str) -> bool {
    let mut rest = trimmed;
    if let Some(after_pub) = rest.strip_prefix("pub") {
        let after_pub = after_pub.trim_start();
        if let Some(after_vis) = after_pub.strip_prefix('(') {
            let Some(close) = after_vis.find(')') else {
                return false;
            };
            rest = after_vis[close + 1..].trim_start();
        } else if after_pub.len() < rest.len() - 3 || rest == "pub" {
            rest = after_pub;
        } else {
            return false;
        }
    }
    let after_kw = if let Some(s) = rest.strip_prefix("const") {
        s
    } else if let Some(s) = rest.strip_prefix("static") {
        s
    } else {
        return false;
    };
    if !after_kw.starts_with(char::is_whitespace) {
        return false;
    }
    let after_kw = after_kw.trim_start();
    !after_kw.starts_with("fn")
        || after_kw["fn".len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn wire_same_file_callbacks(
    conn: &Connection,
    nodes: &HashMap<i64, DeadTailsNode>,
    candidates: &HashSet<i64>,
    test_targeted: &HashSet<i64>,
    fwd_set: &mut HashMap<i64, HashSet<i64>>,
    roots: &mut HashSet<i64>,
) -> Result<()> {
    let mut path_candidates: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut path_all_prod_fns: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut path_test_nodes: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
    let mut paths_needing_scan: HashSet<&str> = HashSet::new();

    for node in nodes.values() {
        if node.is_test_ctx {
            path_test_nodes.entry(node.path.as_str()).or_default().push(node);
            continue;
        }
        if candidates.contains(&node.hash) {
            path_candidates
                .entry(node.path.as_str())
                .or_default()
                .push(node);
            path_all_prod_fns
                .entry(node.path.as_str())
                .or_default()
                .push(node);
            if !roots.contains(&node.hash) {
                paths_needing_scan.insert(node.path.as_str());
            }
        }
    }

    if paths_needing_scan.is_empty() {
        return Ok(());
    }

    let workspace_root: Option<std::path::PathBuf> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'workspace_root'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(std::path::PathBuf::from);

    let mut scanned_on_disk: HashSet<&str> = HashSet::new();
    if let Some(root) = workspace_root.as_deref() {
        let mut source_loader = super::symbols::VerifiedSourceCache::new();
        for &path in &paths_needing_scan {
            if !path.ends_with(".rs") {
                continue;
            }
            let Ok(raw) = source_loader.read(conn, root, path) else {
                continue;
            };
            scanned_on_disk.insert(path);
            let Some(cands) = path_candidates.get(path) else {
                continue;
            };
            let mut by_name: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
            for &cand in cands {
                by_name.entry(cand.name.as_str()).or_default().push(cand);
            }
            let prod_fns = path_all_prod_fns.get(path).map(Vec::as_slice).unwrap_or(&[]);
            let test_nodes = path_test_nodes.get(path).map(Vec::as_slice).unwrap_or(&[]);

            let masked = strip_comments_and_strings_preserving_lines(&raw);
            let mut in_top_level_const = false;
            let mut const_delim_depth: usize = 0;

            for (idx, line_text) in masked.lines().enumerate() {
                let line_num = (idx + 1) as i64;
                if test_nodes
                    .iter()
                    .any(|t| line_num >= t.line && line_num <= t.end_line)
                {
                    in_top_level_const = false;
                    const_delim_depth = 0;
                    continue;
                }
                let trimmed = line_text.trim_start();
                if trimmed.starts_with("#[")
                    || trimmed.starts_with("#![")
                    || trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ")
                    || (trimmed.starts_with("pub(") && trimmed.contains(" use "))
                {
                    continue;
                }
                let enclosing = prod_fns
                    .iter()
                    .copied()
                    .filter(|f| line_num >= f.line && line_num <= f.end_line)
                    .min_by_key(|f| (f.end_line - f.line, f.line, f.hash));

                if enclosing.is_none() && !in_top_level_const && starts_top_level_const_or_static(trimmed) {
                    in_top_level_const = true;
                    const_delim_depth = 0;
                }

                let scan_top_level_const = enclosing.is_none() && in_top_level_const;
                if enclosing.is_some() || scan_top_level_const {
                    scan_bare_callback_tokens(line_text, &mut |ident, has_scope| {
                        let Some(matches) = by_name.get(ident) else {
                            return;
                        };
                        for &target in matches {
                            if !has_scope && target.kind != "function" {
                                continue;
                            }
                            if line_num >= target.line && line_num <= target.end_line {
                                continue;
                            }
                            if let Some(caller) = enclosing {
                                if caller.hash != target.hash {
                                    fwd_set.entry(caller.hash).or_default().insert(target.hash);
                                }
                            } else {
                                roots.insert(target.hash);
                            }
                        }
                    });
                }

                if in_top_level_const {
                    let mut terminated = false;
                    for b in line_text.bytes() {
                        match b {
                            b'(' | b'[' | b'{' => const_delim_depth += 1,
                            b')' | b']' | b'}' => const_delim_depth = const_delim_depth.saturating_sub(1),
                            b';' if const_delim_depth == 0 => terminated = true,
                            _ => {}
                        }
                    }
                    if terminated || enclosing.is_some() {
                        in_top_level_const = false;
                        const_delim_depth = 0;
                    }
                }
            }
        }
    }

    if scanned_on_disk.len() < paths_needing_scan.len() {
        let mut stmt = conn.prepare(
            "SELECT p.path, d.symbol
             FROM dependencies d
             JOIN path_dictionary p ON p.path_id = d.owner_id
             WHERE d.symbol != ''",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            if !paths_needing_scan.contains(path.as_str())
                || scanned_on_disk.contains(path.as_str())
            {
                continue;
            }
            let symbol: String = row.get(1)?;
            if !symbol.contains('(') && !symbol.contains('{') {
                continue;
            }
            let Some(cands) = path_candidates.get(path.as_str()) else {
                continue;
            };
            let mut by_name: HashMap<&str, Vec<&DeadTailsNode>> = HashMap::new();
            for &cand in cands {
                by_name.entry(cand.name.as_str()).or_default().push(cand);
            }
            scan_bare_callback_tokens(&symbol, &mut |ident, has_scope| {
                let Some(matches) = by_name.get(ident) else {
                    return;
                };
                for &target in matches {
                    if !has_scope && target.kind != "function" {
                        continue;
                    }
                    if !test_targeted.contains(&target.hash) {
                        roots.insert(target.hash);
                    }
                }
            });
        }
    }

    Ok(())
}

fn compute_prod_live(graph: &DeadTailsGraph) -> HashSet<i64> {
    let mut live: HashSet<i64> = graph.roots.iter().copied().collect();
    let mut queue: VecDeque<i64> = graph.roots.iter().copied().collect();

    while let Some(u) = queue.pop_front() {
        if let Some(callees) = graph.fwd.get(&u) {
            for &v in callees {
                if graph.candidates.contains(&v) && live.insert(v) {
                    queue.push_back(v);
                }
            }
        }
    }

    live
}

fn collect_test_reachability(
    graph: &DeadTailsGraph,
    dead: &HashSet<i64>,
) -> HashMap<i64, Vec<i64>> {
    let mut callers_of: HashMap<i64, Vec<i64>> = HashMap::new();
    for (&seed, node) in &graph.nodes {
        if !node.is_test_ctx {
            continue;
        }
        let Some(targets) = graph.fwd.get(&seed) else {
            continue;
        };
        let mut seen: HashSet<i64> = HashSet::new();
        let mut queue = VecDeque::new();
        for &target in targets {
            if dead.contains(&target) && seen.insert(target) {
                queue.push_back(target);
            }
        }
        while let Some(u) = queue.pop_front() {
            callers_of.entry(u).or_default().push(seed);
            if let Some(nexts) = graph.fwd.get(&u) {
                for &v in nexts {
                    if dead.contains(&v) && seen.insert(v) {
                        queue.push_back(v);
                    }
                }
            }
        }
    }
    callers_of
}

fn is_dangling_internal_symbol(
    symbol: &str,
    node_names: &HashSet<String>,
    module_names: &HashSet<String>,
) -> bool {
    let Some(rest) = symbol
        .strip_prefix("crate::")
        .or_else(|| symbol.strip_prefix("super::"))
        .or_else(|| symbol.strip_prefix("self::"))
    else {
        return false;
    };
    if rest.is_empty()
        || symbol.contains('(')
        || symbol.contains('.')
        || symbol.contains('\n')
        || symbol.contains('\r')
        || symbol.contains(' ')
    {
        return false;
    }
    let segs: Vec<&str> = rest.split("::").collect();
    let (Some(&first), Some(&last)) = (segs.first(), segs.last()) else {
        return false;
    };
    let is_lower_ident = |s: &str| {
        !s.is_empty()
            && s.as_bytes()[0].is_ascii_lowercase()
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if !is_lower_ident(first) || !is_lower_ident(last) {
        return false;
    }
    if segs
        .iter()
        .any(|s| s.as_bytes().first().is_some_and(u8::is_ascii_uppercase))
    {
        return false;
    }
    !node_names.contains(last) && !module_names.contains(last)
}

fn collect_dangling_calls(
    conn: &Connection,
    scope: &str,
    lower: &str,
    upper: &str,
    graph: &DeadTailsGraph,
) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT p.path, d.symbol, d.kind, d.resolution
         FROM dependencies d
         JOIN path_dictionary p ON p.path_id = d.owner_id
         WHERE d.resolution = 'unresolved'
           AND (?1 = '' OR p.path = ?1 OR (p.path >= ?2 AND p.path < ?3))
           AND (d.symbol GLOB 'crate::*' OR d.symbol GLOB 'super::*' OR d.symbol GLOB 'self::*')
           AND d.symbol NOT GLOB '*(*'
           AND d.symbol NOT GLOB '*.*'",
    )?;
    let mut seen = BTreeSet::new();
    let mut rows = stmt.query(rusqlite::params![scope, lower, upper])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let symbol: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let resolution: String = row.get(3)?;
        if is_dangling_internal_symbol(&symbol, &graph.node_names, &graph.module_names) {
            seen.insert((path, symbol, kind, resolution));
        }
    }
    Ok(seen
        .into_iter()
        .map(|(path, symbol, kind, resolution)| {
            json!({
                "path": path,
                "symbol": symbol,
                "kind": kind,
                "resolution": resolution,
            })
        })
        .collect())
}

fn page_slice(items: Vec<Value>, options: &QueryOptions, generation: &str) -> Value {
    let total = items.len();
    let paged: Vec<Value> = items
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    paging::value(paged, total, options, generation)
}

/// Identifies dead tails, phantom-tested production callables, and unresolved internal calls.
pub fn dead_tails_paged(
    conn: &Connection,
    target: Option<&str>,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let generation = paging::generation(conn, options)?;
    let scope = normalize_scope(target, path, "target")?;
    let (lower, upper) = reader::path_bounds(&scope);

    let graph = load_dead_tails_graph(conn)?;
    let prod_live = compute_prod_live(&graph);
    let dead: HashSet<i64> = graph
        .candidates
        .iter()
        .copied()
        .filter(|nh| !prod_live.contains(nh))
        .collect();
    let test_callers = collect_test_reachability(&graph, &dead);

    let in_scope = |p: &str| -> bool {
        scope.is_empty() || p == scope || (p >= lower.as_str() && p < upper.as_str())
    };

    let mut phantom_nodes: Vec<(&DeadTailsNode, i64)> = Vec::new();
    let mut dead_tail_nodes: Vec<(&DeadTailsNode, i64)> = Vec::new();

    for &nh in &dead {
        let Some(node) = graph.nodes.get(&nh) else {
            continue;
        };
        if !in_scope(&node.path) {
            continue;
        }
        let lines = (node.end_line - node.line + 1).max(1);
        if test_callers.contains_key(&nh) {
            phantom_nodes.push((node, lines));
        } else {
            dead_tail_nodes.push((node, lines));
        }
    }

    let sort_nodes = |a: &(&DeadTailsNode, i64), b: &(&DeadTailsNode, i64)| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.path.cmp(&b.0.path))
            .then_with(|| a.0.line.cmp(&b.0.line))
            .then_with(|| a.0.id.cmp(&b.0.id))
    };
    phantom_nodes.sort_by(sort_nodes);
    dead_tail_nodes.sort_by(sort_nodes);

    let phantom_items: Vec<Value> = phantom_nodes
        .into_iter()
        .map(|(node, lines)| {
            let mut caller_nodes: Vec<&DeadTailsNode> = test_callers
                .get(&node.hash)
                .map(|hashes| {
                    hashes
                        .iter()
                        .filter_map(|h| graph.nodes.get(h))
                        .collect()
                })
                .unwrap_or_default();
            caller_nodes.sort_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let test_caller_count = caller_nodes.len();
            let callers_preview: Vec<Value> = caller_nodes
                .into_iter()
                .take(5)
                .map(|c| {
                    json!({
                        "id": c.id,
                        "name": c.name,
                        "qualname": c.qualname,
                        "path": c.path,
                        "line": c.line,
                    })
                })
                .collect();
            json!({
                "id": node.id,
                "name": node.name,
                "qualname": node.qualname,
                "kind": node.kind,
                "path": node.path,
                "line": node.line,
                "end_line": node.end_line,
                "lines": lines,
                "test_caller_count": test_caller_count,
                "test_callers": callers_preview,
            })
        })
        .collect();

    let dead_tail_items: Vec<Value> = dead_tail_nodes
        .into_iter()
        .map(|(node, lines)| {
            let dead_callees_count = graph
                .fwd
                .get(&node.hash)
                .map_or(0, |callees| callees.iter().filter(|c| dead.contains(*c)).count());
            json!({
                "id": node.id,
                "name": node.name,
                "qualname": node.qualname,
                "kind": node.kind,
                "path": node.path,
                "line": node.line,
                "end_line": node.end_line,
                "lines": lines,
                "dead_callees_count": dead_callees_count,
            })
        })
        .collect();

    let dangling_items = collect_dangling_calls(conn, &scope, &lower, &upper, &graph)?;

    let phantom_count = phantom_items.len();
    let dead_tails_count = dead_tail_items.len();
    let dangling_calls_count = dangling_items.len();

    let phantom_page = page_slice(phantom_items, options, &generation);
    let dead_tails_page = page_slice(dead_tail_items, options, &generation);
    let dangling_page = page_slice(dangling_items, options, &generation);

    Ok(json!({
        "operation": "dead_tails",
        "scope": if scope.is_empty() { None } else { Some(scope) },
        "generation": generation,
        "summary": {
            "phantom_tested_count": phantom_count,
            "dead_tails_count": dead_tails_count,
            "dangling_calls_count": dangling_calls_count,
        },
        "phantom_tested": phantom_page,
        "dead_tails": dead_tails_page,
        "dangling_calls": dangling_page,
    }))
}

fn implementations_paged(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let selected = reader::select_detail_with_path(conn, selector, path, options.detail)?;
    let id = selected["id"].as_str().context("selected node has no id")?;
    let kind = selected["kind"]
        .as_str()
        .context("selected node has no kind")?;
    anyhow::ensure!(
        [
            "class",
            "struct",
            "trait",
            "interface",
            "enum",
            "record",
            "type"
        ]
        .contains(&kind),
        "implementations selector must identify a type"
    );
    let sql = format!("WITH RECURSIVE descendants(hash) AS (SELECT node_hash FROM nodes WHERE id=?1 UNION SELECT e.src_hash FROM descendants d JOIN edges e ON e.dst_hash=d.hash JOIN nodes child ON child.node_hash=e.src_hash WHERE e.kind IN('inherits','implements','extends') AND child.kind IN('class','struct','trait','interface','enum','record','type')) SELECT {} FROM descendants d JOIN nodes n ON n.node_hash=d.hash JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id<>?1 ORDER BY p.path,n.line,n.id", paging::nodes_with_path("n", options.detail, "p.path"));
    Ok(json!({"selector":id,"nodes":paging::query(conn, &sql, &[&id], options)?}))
}

/// Executes graph traversal query with path scoping and graph query options.
pub fn query_with_path_options(
    conn: &Connection,
    operation: &str,
    selector: Option<&str>,
    path: Option<&str>,
    request: &GraphQueryOptions<'_>,
) -> Result<Value> {
    let depth = request.depth;
    let direction = request.direction;
    let coverage = request.coverage;
    let options = request.page;
    let op = operation.trim();
    match op {
        "overview" => reader::overview_with_options(
            conn,
            &reader::OverviewOptions {
                aspects: None,
                page: options,
            },
        ),
        "inspect" => symbols::inspect_paged_response(
            conn,
            selector.unwrap_or(""),
            path,
            true,
            options,
            coverage,
        ),
        "explain" => symbols::explain_paged_response(
            conn,
            selector.unwrap_or(""),
            path,
            None,
            true,
            options,
            coverage,
        ),
        "impact" => traverse_with_path_options(
            conn,
            selector.unwrap_or(""),
            path,
            &TraversalOptions {
                depth,
                inbound: impact_inbound(direction)?,
                mode: None,
                edge_types: None,
                page: options,
            },
        ),
        "slice" => slice_paged(conn, selector.unwrap_or(""), path, depth, options),
        "untested" => untested_paged(conn, selector, path, options),
        "implementations" => implementations_paged(conn, selector.unwrap_or(""), path, options),
        "unwired" => {
            let candidates = "n.kind IN('function','method') AND NOT EXISTS(SELECT 1 FROM edges e WHERE e.dst_hash=n.node_hash AND e.kind='calls')";
            let excluded = paging::count(conn, &format!("SELECT count(*) FROM nodes n WHERE {candidates} AND json_extract(n.details,'$.entrypoint') IS NOT NULL"), &[])?;
            Ok(
                json!({"nodes":paging::query(conn, &format!("SELECT {} FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE {candidates} AND json_extract(n.details,'$.entrypoint') IS NULL ORDER BY p.path,n.line,n.id", paging::nodes_with_path("n", options.detail, "p.path")), &[], options)?, "summary":{"excluded_entrypoints_count":excluded}, "meaning":"no indexed static caller; not a dead-code proof"}),
            )
        }
        "sql" => reader::analyze_paged(conn, sql_selector(selector)?, None, options),
        "doctor" => reader::overview_with_options(
            conn,
            &reader::OverviewOptions {
                aspects: None,
                page: options,
            },
        ),
        "search" | "discover" | "find" => symbols::search_with_options(
            conn,
            selector.unwrap_or(""),
            &symbols::SearchOptions {
                kind: None,
                path,
                include_docs: false,
                exact: false,
                page: options,
            },
        ),
        "duplicates" => {
            let (target, level) = match selector {
                Some("file") | Some("symbol") => (None, selector),
                _ => (selector, direction),
            };
            duplicates_paged(
                conn,
                target,
                path,
                level,
                (depth > 0 && depth != 2).then_some(depth as usize),
                options,
            )
        }
        "dead_tails" => dead_tails_paged(conn, selector, path, options),
        _ => {
            bail!("unknown operation; supported: overview,inspect,explain,impact,slice,unwired,sql,untested,implementations,duplicates,dead_tails")
        }
    }
}
