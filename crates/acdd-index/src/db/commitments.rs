use anyhow::{bail, Context, Result};
use hashbrown::HashMap;
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

mod coverage_leaves;
mod domain_leaves;
mod snapshot;

pub use crate::commitments::{
    aggregate_domain_digest, compute_merkle_root, hash, ALGORITHM, DOMAINS,
};
pub(crate) use snapshot::{fts_commitment_digests, seal_snapshot_from_projection, SnapshotTimings};
const FTS: &[(&str, &str)] = &[
    ("node_search_postings", "term,doc,col,offset"),
    ("node_search_config", "k"),
    ("doc_search_postings", "term,doc,col,offset"),
    ("doc_search_config", "k"),
];
fn encode_row(
    hasher: &mut impl sha2::digest::Update,
    row: &rusqlite::Row<'_>,
    columns: usize,
) -> Result<()> {
    hasher.update(&[0xff]);
    for i in 0..columns {
        match row.get_ref(i)? {
            ValueRef::Null => hasher.update(b"n"),
            ValueRef::Integer(n) => {
                hasher.update(b"i");
                hasher.update(&n.to_le_bytes());
            }
            ValueRef::Real(n) => {
                hasher.update(b"r");
                hasher.update(&n.to_bits().to_le_bytes());
            }
            ValueRef::Text(s) => {
                hasher.update(b"t");
                hasher.update(&(s.len() as u64).to_le_bytes());
                hasher.update(s);
            }
            ValueRef::Blob(s) => {
                hasher.update(b"b");
                hasher.update(&(s.len() as u64).to_le_bytes());
                hasher.update(s);
            }
        }
    }
    Ok(())
}
fn append_text(buffer: &mut Vec<u8>, text: &[u8]) {
    buffer.push(b't');
    buffer.extend_from_slice(&(text.len() as u64).to_le_bytes());
    buffer.extend_from_slice(text);
}
fn append_integer(buffer: &mut Vec<u8>, value: i64) {
    buffer.push(b'i');
    buffer.extend_from_slice(&value.to_le_bytes());
}
fn append_fts_posting(
    buffer: &mut Vec<u8>,
    term: &[u8],
    identity: FtsRowIdentityRef<'_>,
    column: &[u8],
    offset: i64,
) {
    buffer.push(0xff);
    append_text(buffer, term);
    match identity {
        FtsRowIdentityRef::NodeHash(node_hash) => append_integer(buffer, node_hash),
        FtsRowIdentityRef::DocId(doc_id) => append_text(buffer, doc_id),
    }
    append_text(buffer, column);
    append_integer(buffer, offset);
}
pub(crate) fn new_leaf_hasher(table: &str, owner: &str) -> Sha256 {
    let mut h = Sha256::new();
    h.update(ALGORITHM);
    h.update(table);
    h.update((owner.len() as u64).to_le_bytes());
    h.update(owner.as_bytes());
    h
}
/// Formats leaf commitment key for a table and owner.
pub fn key(table: &str, owner: &str) -> String {
    format!("leaf:{}", serde_json::json!([table, owner]))
}
fn shape(table: &str) -> (&'static str, &'static str, &'static str) {
    match table {
        "errors" => ("t.path", "", "t.line,t.message"),
        "doc_sections" => ("t.path", "", "t.doc_id"),
        _ => ("t.path", "", "t.path"),
    }
}
#[inline]
pub(crate) fn encode_text(h: &mut impl sha2::digest::Update, s: &str) {
    encode_text_bytes(h, s.as_bytes());
}

#[inline]
pub(crate) fn encode_text_bytes(h: &mut impl sha2::digest::Update, s: &[u8]) {
    h.update(b"t");
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s);
}

#[inline]
pub(crate) fn encode_int(h: &mut impl sha2::digest::Update, n: i64) {
    h.update(b"i");
    h.update(&n.to_le_bytes());
}

#[inline]
pub(crate) fn encode_real(h: &mut impl sha2::digest::Update, value: f64) {
    h.update(b"r");
    h.update(&value.to_bits().to_le_bytes());
}

#[inline]
pub(crate) fn encode_blob(h: &mut impl sha2::digest::Update, value: &[u8]) {
    h.update(b"b");
    h.update(&(value.len() as u64).to_le_bytes());
    h.update(value);
}

#[inline]
pub(crate) fn encode_null(h: &mut impl sha2::digest::Update) {
    h.update(b"n");
}

#[derive(Clone, Copy)]
struct TextSpan {
    start: usize,
    end: usize,
}

#[derive(Default)]
struct TextArena(String);

impl TextArena {
    fn push(&mut self, text: &str) -> TextSpan {
        let start = self.0.len();
        self.0.push_str(text);
        TextSpan {
            start,
            end: self.0.len(),
        }
    }

    fn get(&self, span: TextSpan) -> &str {
        &self.0[span.start..span.end]
    }
}

fn leaves(
    conn: &Connection,
    table: &str,
    owners: Option<&BTreeSet<String>>,
) -> Result<BTreeMap<String, String>> {
    match table {
        "nodes" => return domain_leaves::leaves_nodes(conn, owners),
        "edge_occurrences" => return domain_leaves::leaves_edge_occurrences(conn, owners),
        "edges" => return domain_leaves::leaves_edges(conn, owners),
        "shared_owners" => return domain_leaves::leaves_shared_owners(conn, owners),
        "dependencies" => return domain_leaves::leaves_dependencies(conn, owners),
        "resolution_coverage" => {
            if let Some(leaves) = coverage_leaves::canonical_coverage_leaves(conn, owners)? {
                return Ok(leaves);
            }
        }
        "coverage_owner_language" => {
            let leaves = coverage_leaves::canonical_owner_language_leaves(conn, owners)?
                .context("coverage_owner_language rows require canonical text")?;
            return Ok(leaves);
        }
        "coverage_language_counts" => {
            let leaves = coverage_leaves::canonical_language_count_leaves(conn, owners)?
                .context("coverage_language_counts rows require canonical text")?;
            return Ok(leaves);
        }
        _ => {}
    }
    domain_leaves::leaves_generic(conn, table, owners)
}

fn ensure_fts_vocab(conn: &Connection, index: &str) -> Result<&'static str> {
    let (table, create_sql) = match index {
        "node_search" => (
            "node_search_commit_vocab",
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.node_search_commit_vocab \
             USING fts5vocab(main,node_search,'instance')",
        ),
        "doc_search" => (
            "doc_search_commit_vocab",
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.doc_search_commit_vocab \
             USING fts5vocab(main,doc_search,'instance')",
        ),
        _ => anyhow::bail!("unsupported FTS commitment index: {index}"),
    };
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(table);
    }

    // Snapshot hashing opens read-only connections and marks them query-only.
    // SQLite permits a temp vocab view on a read-only main database, but only
    // when query_only is briefly disabled while the temp object is created.
    let query_only: i64 = conn.query_row("PRAGMA query_only", [], |row| row.get(0))?;
    if query_only != 0 {
        conn.pragma_update(None, "query_only", false)?;
    }
    let create_result = conn.execute_batch(create_sql);
    let restore_result = if query_only != 0 {
        conn.pragma_update(None, "query_only", true)
    } else {
        Ok(())
    };
    create_result?;
    restore_result?;
    Ok(table)
}

fn decode_fts5_size_array(bytes: &[u8], columns: usize) -> Result<Vec<u32>> {
    let mut offset = 0;
    let mut sizes = Vec::with_capacity(columns);
    for _ in 0..columns {
        let mut value = 0u32;
        let mut encoded_bytes = 0;
        loop {
            let byte = *bytes
                .get(offset)
                .context("truncated FTS5 document-size vector")?;
            offset += 1;
            encoded_bytes += 1;
            anyhow::ensure!(encoded_bytes <= 5, "oversized FTS5 document-size value");
            value = value
                .checked_mul(128)
                .and_then(|value| value.checked_add(u32::from(byte & 0x7f)))
                .context("FTS5 document-size value overflow")?;
            if byte & 0x80 == 0 {
                break;
            }
        }
        sizes.push(value);
    }
    anyhow::ensure!(
        offset == bytes.len(),
        "FTS5 document-size vector has trailing bytes"
    );
    Ok(sizes)
}

struct FtsCommitmentSpec {
    docsize_table: &'static str,
    fts_table: &'static str,
    owner_join: &'static str,
    identity: &'static str,
    identity_kind: FtsIdentityKind,
    columns: &'static [&'static str],
}

#[derive(Clone, Copy)]
enum FtsIdentityKind {
    NodeHash,
    DocId,
}

enum FtsRowIdentity {
    NodeHash(i64),
    DocId(Vec<u8>),
}

enum FtsRowIdentityRef<'a> {
    NodeHash(i64),
    DocId(&'a [u8]),
}

struct FtsDocument {
    identity_sort_key: Option<i64>,
    size_blob: Vec<u8>,
    token_counts: [u32; 3],
}

type FtsDocumentMap = HashMap<i64, FtsDocument>;
type FtsOwnerIds = Vec<Vec<u8>>;

struct FtsPosting {
    term: u32,
    identity_sort_key: i64,
    column_index: u8,
    column_sort_key: u8,
    offset: i64,
}

fn fts_commitment_spec(index: &str) -> Result<FtsCommitmentSpec> {
    Ok(match index {
        "node_search" => FtsCommitmentSpec {
            docsize_table: "node_search_docsize",
            fts_table: "node_search",
            owner_join: "LEFT JOIN nodes AS n ON n.node_hash=d.id",
            identity: "n.node_hash",
            identity_kind: FtsIdentityKind::NodeHash,
            columns: &["search_text"],
        },
        "doc_search" => FtsCommitmentSpec {
            docsize_table: "doc_search_docsize",
            fts_table: "doc_search",
            owner_join: "LEFT JOIN doc_sections AS s ON s.rowid=d.id",
            identity: "s.doc_id",
            identity_kind: FtsIdentityKind::DocId,
            columns: &["section_title", "content", "invariants"],
        },
        _ => anyhow::bail!("unsupported FTS commitment index: {index}"),
    })
}

fn load_fts_documents(
    conn: &Connection,
    spec: &FtsCommitmentSpec,
) -> Result<(FtsDocumentMap, FtsOwnerIds)> {
    let mut stmt = conn.prepare(&format!(
        "SELECT d.id,{identity},d.sz
         FROM {docsize} AS d
         {owner_join}",
        identity = spec.identity,
        owner_join = spec.owner_join,
        docsize = spec.docsize_table,
    ))?;
    let mut rows = stmt.query([])?;
    let mut documents = Vec::new();
    while let Some(row) = rows.next()? {
        let doc: i64 = row.get(0)?;
        let identity = match (spec.identity_kind, row.get_ref(1)?) {
            (FtsIdentityKind::NodeHash, ValueRef::Integer(node_hash)) => {
                Some(FtsRowIdentity::NodeHash(node_hash))
            }
            (FtsIdentityKind::DocId, ValueRef::Text(doc_id)) => {
                Some(FtsRowIdentity::DocId(doc_id.to_vec()))
            }
            (_, ValueRef::Null) => None,
            _ => anyhow::bail!("invalid FTS5 owner identity: {}:{doc}", spec.fts_table),
        };
        let size_blob: Option<Vec<u8>> = row.get(2)?;
        let size_blob = size_blob.with_context(|| {
            format!(
                "missing active FTS5 docsize row: {}:{doc}",
                spec.docsize_table
            )
        })?;
        documents.push((doc, identity, size_blob));
    }

    let mut owner_ids: Vec<Vec<u8>> = documents
        .iter()
        .filter_map(|(_, identity, _)| match identity {
            Some(FtsRowIdentity::DocId(doc_id)) => Some(doc_id.clone()),
            _ => None,
        })
        .collect();
    owner_ids.sort_unstable();
    owner_ids.dedup();
    let owner_ranks: HashMap<Vec<u8>, i64> = owner_ids
        .iter()
        .enumerate()
        .map(|(rank, doc_id)| Ok((doc_id.clone(), i64::try_from(rank)?)))
        .collect::<Result<_>>()?;

    let mut by_doc = HashMap::with_capacity(documents.len());
    for (doc, identity, size_blob) in documents {
        let identity_sort_key = match identity.as_ref() {
            Some(FtsRowIdentity::NodeHash(node_hash)) => Some(*node_hash),
            Some(FtsRowIdentity::DocId(doc_id)) => owner_ranks.get(doc_id).copied(),
            None => None,
        };
        by_doc.insert(
            doc,
            FtsDocument {
                identity_sort_key,
                size_blob,
                token_counts: [0; 3],
            },
        );
    }
    Ok((by_doc, owner_ids))
}

fn validate_fts_docsize(
    spec: &FtsCommitmentSpec,
    documents: &HashMap<i64, FtsDocument>,
) -> Result<()> {
    for (doc, document) in documents {
        let actual =
            decode_fts5_size_array(&document.size_blob, spec.columns.len()).with_context(|| {
                format!("invalid FTS5 document sizes: {}:{doc}", spec.docsize_table)
            })?;
        for (column_index, column) in spec.columns.iter().enumerate() {
            anyhow::ensure!(
                actual[column_index] == document.token_counts[column_index],
                "FTS5 document size mismatch: {}:{doc} column {column}",
                spec.docsize_table
            );
        }
    }
    Ok(())
}

const FTS5_STRUCTURE_ROWID: i64 = 10;
const FTS5_STRUCTURE_V2: &[u8; 4] = b"\xff\x00\x00\x01";
const FTS5_MAX_SEGMENT: u32 = 2000;

fn fts5_segment_rowid(segid: i64, pgno: i64) -> Option<i64> {
    if !(1..=65535).contains(&segid) || !(0..(1i64 << 31)).contains(&pgno) { return None; }
    segid.checked_shl(37)?.checked_add(pgno)
}

fn decode_fts5_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut val = 0u64;
    for i in 0..8 {
        let b = *bytes.get(i)?;
        val = (val << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((val, i + 1));
        }
    }
    let b8 = *bytes.get(8)?;
    Some(((val << 8) | u64::from(b8), 9))
}

fn decode_fts5_varint32(bytes: &[u8]) -> Option<(u32, usize)> {
    let (v, n) = decode_fts5_varint(bytes)?;
    Some((v as u32, n))
}

fn next_v32(s: &[u8], i: &mut usize) -> Option<u32> {
    let (value, n) = decode_fts5_varint32(s.get(*i..)?)?;
    *i += n;
    Some(value)
}

fn next_v64(s: &[u8], i: &mut usize) -> Option<u64> {
    let (value, n) = decode_fts5_varint(s.get(*i..)?)?;
    *i += n;
    Some(value)
}

fn decode_fts5_structure_segments(s: &[u8]) -> Option<Vec<(i64, i64, i64)>> {
    if s.len() < 4 {
        return None;
    }
    let mut i = 4usize;
    let v2 = s.get(i..i + 4) == Some(FTS5_STRUCTURE_V2);
    if v2 {
        i += 4;
    }
    let n_level = next_v32(s, &mut i)?;
    let mut n_segment = next_v32(s, &mut i)?;
    if n_level > FTS5_MAX_SEGMENT || n_segment > FTS5_MAX_SEGMENT {
        return None;
    }
    let _write_counter = next_v64(s, &mut i)?;
    let mut segments = Vec::with_capacity(n_segment as usize);
    for _ in 0..n_level {
        let n_merge = next_v32(s, &mut i)?;
        let n_total = next_v32(s, &mut i)?;
        if n_total < n_merge || n_segment < n_total {
            return None;
        }
        n_segment -= n_total;
        let level_start = segments.len();
        for _ in 0..n_total {
            let segid = next_v32(s, &mut i)?;
            let pgno_first = next_v32(s, &mut i)?;
            let pgno_last = next_v32(s, &mut i)?;
            if v2 {
                let _ = next_v64(s, &mut i)?;
                let _ = next_v64(s, &mut i)?;
                let n_pg_tomb = next_v32(s, &mut i)?;
                let n_entry_tomb = next_v64(s, &mut i)?;
                let _ = next_v64(s, &mut i)?;
                if n_pg_tomb > 0 || n_entry_tomb > 0 {
                    return None;
                }
            }
            if segid == 0 || segid > 65535 || pgno_last < pgno_first || pgno_last >= (1 << 31) {
                return None;
            }
            if pgno_first > 0 {
                segments.push((i64::from(segid), i64::from(pgno_first), i64::from(pgno_last)));
            }
        }
        // Within each level (0 = newest level), later segments are newer (`fts5MultiIterNew`).
        segments[level_start..].reverse();
    }
    (n_segment == 0 && i == s.len()).then_some(segments)
}

struct Fts5SegCursor<'a> {
    pages: &'a [Vec<u8>],
    leaf: usize,
    off: usize,
    pgidx: usize,
    eod: usize,
    term: Vec<u8>,
    rowid: i64,
}

impl Fts5SegCursor<'_> {
    fn page_sz(p: &[u8]) -> Option<usize> {
        Some(usize::from(u16::from_be_bytes([*p.get(2)?, *p.get(3)?])))
    }

    fn next_page(&mut self) -> Option<bool> {
        self.leaf += 1;
        let p = self.pages.get(self.leaf)?;
        let sz = Self::page_sz(p)?;
        if !(4..=p.len()).contains(&sz) {
            return None;
        }
        self.pgidx = sz;
        self.eod = if sz >= p.len() {
            p.len() + 1
        } else {
            usize::try_from(next_v32(p, &mut self.pgidx)?).ok()?
        };
        Some(true)
    }

    fn load_term(&mut self, n_keep: usize) -> Option<()> {
        let p = self.pages.get(self.leaf)?;
        let sz = Self::page_sz(p)?;
        if !(4..=p.len()).contains(&sz) || self.off >= sz {
            return None;
        }
        let n_new = usize::try_from(next_v32(&p[..sz], &mut self.off)?).ok()?;
        if n_new == 0 || n_keep > self.term.len() || self.off + n_new > sz {
            return None;
        }
        self.term.truncate(n_keep);
        self.term.extend_from_slice(&p[self.off..self.off + n_new]);
        self.off += n_new;
        if self.pgidx >= p.len() {
            self.eod = p.len() + 1;
        } else {
            self.eod = self.eod.checked_add(usize::try_from(next_v32(p, &mut self.pgidx)?).ok()?)?;
        }
        loop {
            let p = self.pages.get(self.leaf)?;
            let sz = Self::page_sz(p)?;
            if !(4..=p.len()).contains(&sz) {
                return None;
            }
            if self.off < sz {
                self.rowid = next_v64(&p[..sz], &mut self.off)? as i64;
                break;
            }
            self.next_page()?;
            self.off = 4;
        }
        Some(())
    }

    fn load_n_pos(&mut self) -> Option<(usize, bool)> {
        let p = self.pages.get(self.leaf)?;
        let sz = Self::page_sz(p)?;
        if self.off >= sz || sz > p.len() {
            return None;
        }
        let n = next_v32(&p[..sz], &mut self.off)?;
        Some((usize::try_from(n >> 1).ok()?, n & 1 != 0))
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_fts5_segment_pages(
    pages: &[Vec<u8>],
    multi_segment: bool,
    seen_term_docs: &mut hashbrown::HashSet<(u32, i64)>,
    term_map: &mut HashMap<Vec<u8>, u32>,
    terms: &mut Vec<Vec<u8>>,
    last_term: &mut Vec<u8>,
    last_term_id: &mut u32,
    documents: &mut FtsDocumentMap,
    column_sort_keys: &[u8],
    num_columns: usize,
    postings: &mut Vec<FtsPosting>,
) -> Option<()> {
    let Some(leaf_idx) = pages.iter().position(|page| page.len() != 4) else {
        return Some(());
    };
    let first_page = &pages[leaf_idx];
    let sz_leaf0 = Fts5SegCursor::page_sz(first_page)?;
    if sz_leaf0 < 4 || sz_leaf0 >= first_page.len() {
        return None;
    }
    let (first_term_off, n_ft) = decode_fts5_varint32(first_page.get(sz_leaf0..)?)?;
    if first_term_off != 4 {
        return None;
    }
    let mut cursor = Fts5SegCursor {
        pages,
        leaf: leaf_idx,
        off: 4,
        pgidx: sz_leaf0 + n_ft,
        eod: usize::try_from(first_term_off).ok()?,
        term: Vec::new(),
        rowid: 0,
    };
    cursor.load_term(0)?;
    let (mut n_pos, mut b_del) = cursor.load_n_pos()?;
    let mut poslist = Vec::new();

    loop {
        let Some(vocab_term) = cursor.term.strip_prefix(b"0") else {
            if cursor.term.first().is_some_and(|&b| b > b'0') {
                break;
            }
            return None;
        };

        poslist.clear();
        let (mut n_rem, mut cur_idx, mut chunk_start) = (n_pos, cursor.leaf, cursor.off);
        while n_rem > 0 {
            let p = pages.get(cur_idx)?;
            let sz = Fts5SegCursor::page_sz(p)?;
            if chunk_start > sz || sz > p.len() {
                return None;
            }
            let take = n_rem.min(sz - chunk_start);
            poslist.extend_from_slice(&p[chunk_start..chunk_start + take]);
            n_rem -= take;
            if n_rem > 0 {
                cur_idx += 1;
                chunk_start = 4;
            }
        }

        if terms.is_empty() || vocab_term != last_term.as_slice() {
            *last_term_id = match term_map.get(vocab_term) {
                Some(&id) => id,
                None => {
                    let id = u32::try_from(terms.len()).ok()?;
                    let owned = vocab_term.to_vec();
                    term_map.insert(owned.clone(), id);
                    terms.push(owned);
                    id
                }
            };
            last_term.clear();
            last_term.extend_from_slice(vocab_term);
        }
        let term_id = *last_term_id;

        let is_newest_for_term_doc = !multi_segment || seen_term_docs.insert((term_id, cursor.rowid));
        if is_newest_for_term_doc && !b_del {
            let document = documents.get_mut(&cursor.rowid)?;
            let identity_sort_key = document.identity_sort_key?;

            let mut pos_off = 0usize;
            let (mut cur_col, mut cur_off) = (0u32, 0u32);
            while pos_off < poslist.len() {
                let val = next_v32(&poslist, &mut pos_off)?;
                if val == 0 { return None; }
                if val == 1 {
                    cur_col = next_v32(&poslist, &mut pos_off)?;
                    cur_off = next_v32(&poslist, &mut pos_off)?.checked_sub(2)?;
                } else {
                    cur_off = cur_off.checked_add(val - 2)?;
                }
                let col_idx = usize::try_from(cur_col).ok()?;
                if col_idx >= num_columns { return None; }
                let offset = i64::from(cur_off);
                document.token_counts[col_idx] = document.token_counts[col_idx].checked_add(1)?;
                postings.push(FtsPosting {
                    term: term_id,
                    identity_sort_key,
                    column_index: u8::try_from(col_idx).ok()?,
                    column_sort_key: column_sort_keys[col_idx],
                    offset,
                });
            }
        }

        let p = pages.get(cursor.leaf)?;
        let sz_leaf = Fts5SegCursor::page_sz(p)?;
        let mut i_off = cursor.off.checked_add(n_pos)?;
        if i_off < sz_leaf {
            if i_off >= cursor.eod {
                let (ft_off, _) = decode_fts5_varint32(p.get(sz_leaf..)?)?;
                let n_keep = if i_off != usize::try_from(ft_off).ok()? {
                    usize::try_from(next_v32(&p[..sz_leaf], &mut i_off)?).ok()?
                } else {
                    0
                };
                cursor.off = i_off;
                cursor.load_term(n_keep)?;
            } else {
                let delta = next_v64(&p[..sz_leaf], &mut i_off)?;
                if delta == 0 {
                    return None;
                }
                cursor.rowid = cursor.rowid.checked_add(i64::try_from(delta).ok()?)?;
                cursor.off = i_off;
            }
            (n_pos, b_del) = cursor.load_n_pos()?;
        } else {
            let mut rem = i_off - sz_leaf;
            let found_next = loop {
                if cursor.leaf + 1 >= pages.len() { break false; }
                cursor.next_page()?;
                let np = &pages[cursor.leaf];
                let sz = Fts5SegCursor::page_sz(np)?;
                if rem >= sz - 4 { rem -= sz - 4; continue; }
                let mut landing = 4 + rem;
                if landing >= cursor.eod {
                    let (ft_off, _) = decode_fts5_varint32(np.get(sz..)?)?;
                    let n_keep = if landing == usize::try_from(ft_off).ok()? { 0 } else {
                        usize::try_from(next_v32(&np[..sz], &mut landing)?).ok()?
                    };
                    cursor.off = landing;
                    cursor.load_term(n_keep)?;
                } else {
                    let first = usize::from(u16::from_be_bytes([np[0], np[1]]));
                    let value = next_v64(&np[..sz], &mut landing)?;
                    cursor.rowid = if 4 + rem == first { value as i64 } else {
                        if value == 0 { return None; }
                        cursor.rowid.checked_add(i64::try_from(value).ok()?)?
                    };
                    cursor.off = landing;
                }
                break true;
            };
            if !found_next {
                if rem > 0 { return None; }
                break;
            }
            (n_pos, b_del) = cursor.load_n_pos()?;
        }
    }
    Some(())
}

/// Computes the canonical FTS digest directly from SQLite FTS5 `*_data` / `*_idx` shadow tables.
/// Returns `Ok(None)` when the index requires `fts_digest_vocab` fallback.
pub fn fts_digest_fastpath(conn: &Connection, table: &str) -> Result<Option<String>> {
    let table = match table {
        "node_search" => "node_search_postings",
        "doc_search" => "doc_search_postings",
        _ => table,
    };
    let Some(index) = table.strip_suffix("_postings") else {
        return Ok(None);
    };
    let vocab_table = match index {
        "node_search" => "node_search_commit_vocab",
        "doc_search" => "doc_search_commit_vocab",
        _ => return Ok(None),
    };
    let has_non_virtual_override: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name=?1 AND sql NOT LIKE 'CREATE VIRTUAL TABLE%')",
        [vocab_table],
        |row| row.get(0),
    )?;
    if has_non_virtual_override {
        return Ok(None);
    }
    let spec = fts_commitment_spec(index)?;
    let mut config = conn.prepare(&format!("SELECT k,v FROM {index}_config WHERE k IN ('version','detail')"))?;
    let mut rows = config.query([])?;
    let mut version_ok = false;
    while let Some(row) = rows.next()? {
        match (row.get::<_, String>(0)?.as_str(), row.get_ref(1)?) {
            ("version", ValueRef::Integer(4) | ValueRef::Text(b"4")) => version_ok = true,
            ("detail", ValueRef::Text(b"full")) => (),
            _ => return Ok(None),
        }
    }
    if !version_ok { return Ok(None); }
    drop(rows);
    drop(config);
    let _: Option<i64> = conn.query_row(
        &format!("SELECT segid FROM {index}_idx LIMIT 1"), [], |row| row.get(0),
    ).optional()?;
    let Some(structure_blob) = conn.query_row(
        &format!("SELECT block FROM {index}_data WHERE id=?1"),
        [FTS5_STRUCTURE_ROWID],
        |row| match row.get_ref(0)? {
            ValueRef::Blob(b) => Ok(Some(b.to_vec())),
            _ => Ok(None),
        },
    ).optional()?.flatten() else { return Ok(None); };
    let Some(segments) = decode_fts5_structure_segments(&structure_blob) else {
        return Ok(None);
    };
    let (mut documents, owner_ids) = load_fts_documents(conn, &spec)?;
    if documents.values().any(|doc| doc.identity_sort_key.is_none()) {
        return Ok(None);
    }
    let mut column_order: Vec<usize> = (0..spec.columns.len()).collect();
    column_order.sort_unstable_by_key(|column| spec.columns[*column].as_bytes());
    let mut column_sort_keys = vec![0; spec.columns.len()];
    for (rank, column) in column_order.into_iter().enumerate() {
        column_sort_keys[column] = u8::try_from(rank)?;
    }

    let mut term_map: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut terms: Vec<Vec<u8>> = Vec::new();
    let mut last_term: Vec<u8> = Vec::new();
    let mut last_term_id: u32 = 0;
    let mut postings = Vec::new();
    let mut seen_term_docs: hashbrown::HashSet<(u32, i64)> = hashbrown::HashSet::new();
    let multi_segment = segments.len() > 1;

    let mut page_stmt = conn.prepare(&format!(
        "SELECT id, block FROM {index}_data WHERE id >= ?1 AND id <= ?2 ORDER BY id ASC"
    ))?;
    for (segid, pgno_first, pgno_last) in segments {
        let (Some(first_id), Some(last_id)) = (fts5_segment_rowid(segid, pgno_first), fts5_segment_rowid(segid, pgno_last)) else { return Ok(None); };
        let expected_count = match usize::try_from(pgno_last - pgno_first + 1) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let mut rows = page_stmt.query(params![first_id, last_id])?;
        let mut pages = Vec::with_capacity(expected_count.min(1024));
        let mut expected_id = first_id;
        while let Some(row) = rows.next()? {
            let row_id: i64 = row.get(0)?;
            let ValueRef::Blob(blob) = row.get_ref(1)? else {
                return Ok(None);
            };
            if row_id != expected_id {
                return Ok(None);
            }
            pages.push(blob.to_vec());
            expected_id += 1;
        }
        if pages.len() != expected_count {
            return Ok(None);
        }
        if decode_fts5_segment_pages(
            &pages,
            multi_segment,
            &mut seen_term_docs,
            &mut term_map,
            &mut terms,
            &mut last_term,
            &mut last_term_id,
            &mut documents,
            &column_sort_keys,
            spec.columns.len(),
            &mut postings,
        )
        .is_none()
        {
            return Ok(None);
        }
    }
    drop(page_stmt);

    let mut h = Sha256::new();
    h.update(table);
    if let Err(err) =
        finalize_fts_postings_digest(&mut h, &spec, &documents, &owner_ids, terms, postings)
    {
        if !conn.is_autocommit() {
            return Ok(None);
        }
        return Err(err);
    }
    Ok(Some(hex::encode(h.finalize())))
}

fn fts_digest(conn: &Connection, table: &str, order_by: &str) -> Result<String> {
    if !matches!(table, "node_search_postings" | "doc_search_postings") {
        return fts_digest_vocab(conn, table, order_by);
    }
    if let Some(index) = table.strip_suffix("_postings") {
        let vocab = ensure_fts_vocab(conn, index)?;
        let _: Option<String> = conn.query_row(&format!("SELECT term FROM {vocab} LIMIT 1"), [], |row| row.get(0)).optional()?;
    }
    let snapshot = if conn.is_autocommit() {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    let digest = match fts_digest_fastpath(conn, table)? {
        Some(digest) => digest,
        None => fts_digest_vocab(conn, table, order_by)?,
    };
    if let Some(snapshot) = snapshot {
        snapshot.commit()?;
    }
    Ok(digest)
}

fn finalize_fts_postings_digest(
    h: &mut Sha256,
    spec: &FtsCommitmentSpec,
    documents: &FtsDocumentMap,
    owner_ids: &FtsOwnerIds,
    terms: Vec<Vec<u8>>,
    mut postings: Vec<FtsPosting>,
) -> Result<()> {
    let terms = if terms.windows(2).all(|w| w[0] < w[1]) {
        terms
    } else {
        let mut sorted_indexed: Vec<(u32, Vec<u8>)> = terms
            .into_iter()
            .enumerate()
            .map(|(i, t)| (i as u32, t))
            .collect();
        sorted_indexed.sort_unstable_by(|a, b| a.1.cmp(&b.1));
        let mut remap = vec![0u32; sorted_indexed.len()];
        let mut sorted_terms = Vec::with_capacity(sorted_indexed.len());
        for (new_rank, (old_id, term)) in sorted_indexed.into_iter().enumerate() {
            remap[old_id as usize] = u32::try_from(new_rank)?;
            sorted_terms.push(term);
        }
        for posting in &mut postings {
            posting.term = remap[posting.term as usize];
        }
        sorted_terms
    };

    postings.sort_unstable_by_key(|posting| {
        (
            posting.term,
            posting.identity_sort_key,
            posting.column_sort_key,
            posting.offset,
        )
    });
    let mut encoded_rows = Vec::with_capacity(256 * 1024);
    for posting in postings {
        let identity = match spec.identity_kind {
            FtsIdentityKind::NodeHash => FtsRowIdentityRef::NodeHash(posting.identity_sort_key),
            FtsIdentityKind::DocId => {
                let rank = usize::try_from(posting.identity_sort_key)?;
                FtsRowIdentityRef::DocId(
                    owner_ids
                        .get(rank)
                        .context("sorted FTS5 posting lost its document identity")?,
                )
            }
        };
        append_fts_posting(
            &mut encoded_rows,
            terms
                .get(posting.term as usize)
                .context("sorted FTS5 posting lost its term")?,
            identity,
            spec.columns[posting.column_index as usize].as_bytes(),
            posting.offset,
        );
        if encoded_rows.len() >= 256 * 1024 {
            h.update(&encoded_rows);
            encoded_rows.clear();
        }
    }
    h.update(&encoded_rows);
    validate_fts_docsize(spec, documents)?;
    Ok(())
}

/// Computes the canonical FTS digest using SQLite's instance vocabulary.
pub fn fts_digest_vocab(conn: &Connection, table: &str, order_by: &str) -> Result<String> {
    let table = match table {
        "node_search" => "node_search_postings",
        "doc_search" => "doc_search_postings",
        _ => table,
    };
    let mut h = Sha256::new();
    h.update(table);
    if let Some(index) = table.strip_suffix("_postings") {
        let vocab = ensure_fts_vocab(conn, index)?;
        let spec = fts_commitment_spec(index)?;
        let (mut documents, owner_ids) = load_fts_documents(conn, &spec)?;
        let mut column_order: Vec<usize> = (0..spec.columns.len()).collect();
        column_order.sort_unstable_by_key(|column| spec.columns[*column].as_bytes());
        let mut column_sort_keys = vec![0; spec.columns.len()];
        for (rank, column) in column_order.into_iter().enumerate() {
            column_sort_keys[column] = u8::try_from(rank)?;
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT v.term,v.col,v.offset,v.doc FROM temp.{vocab} AS v"
        ))?;
        let mut rows = stmt.query([])?;
        let mut term_map: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut terms: Vec<Vec<u8>> = Vec::new();
        let mut last_term: Vec<u8> = Vec::new();
        let mut last_term_id: u32 = 0;
        let mut postings = Vec::new();
        while let Some(row) = rows.next()? {
            let term_bytes = match row.get_ref(0)? {
                ValueRef::Text(term) => term,
                _ => anyhow::bail!("invalid FTS5 vocabulary term in {table}"),
            };
            let term_id = if !terms.is_empty() && term_bytes == last_term.as_slice() {
                last_term_id
            } else if let Some(&id) = term_map.get(term_bytes) {
                last_term.clear();
                last_term.extend_from_slice(term_bytes);
                last_term_id = id;
                id
            } else {
                let id = u32::try_from(terms.len())?;
                let owned = term_bytes.to_vec();
                term_map.insert(owned.clone(), id);
                last_term.clear();
                last_term.extend_from_slice(term_bytes);
                last_term_id = id;
                terms.push(owned);
                id
            };
            let column = match row.get_ref(1)? {
                ValueRef::Text(column) => column,
                _ => anyhow::bail!("invalid FTS5 document-size column in {table}"),
            };
            let column_index = spec
                .columns
                .iter()
                .position(|candidate| candidate.as_bytes() == column)
                .with_context(|| {
                    format!(
                        "unknown FTS5 document-size column: {index}:{}",
                        String::from_utf8_lossy(column)
                    )
                })?;
            let doc: i64 = row.get(3)?;
            let document = documents
                .get_mut(&doc)
                .with_context(|| format!("FTS5 postings refer to missing owner rows: {table}"))?;
            let identity_sort_key = document
                .identity_sort_key
                .with_context(|| format!("FTS5 postings refer to missing owner rows: {table}"))?;
            document.token_counts[column_index] = document.token_counts[column_index]
                .checked_add(1)
                .context("FTS5 token count exceeds document-size range")?;
            postings.push(FtsPosting {
                term: term_id,
                identity_sort_key,
                column_index: u8::try_from(column_index)?,
                column_sort_key: column_sort_keys[column_index],
                offset: row.get(2)?,
            });
        }
        drop(rows);
        drop(stmt);

        finalize_fts_postings_digest(&mut h, &spec, &documents, &owner_ids, terms, postings)?;
    } else {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY {order_by}"))?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            encode_row(&mut h, row, columns)?;
        }
    }
    Ok(hex::encode(h.finalize()))
}
fn root(conn: &Connection) -> Result<String> {
    let mut domains = Vec::new();
    let mut stmt = conn.prepare("SELECT domain,CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END FROM domain_commitments WHERE owner_id=0 ORDER BY domain")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        domains.push(row?);
    }
    let mut metadata = Vec::new();
    let mut stmt = conn.prepare("SELECT key,value FROM metadata WHERE key IN('schema_version','index_semantics_version','indexer_engine','workspace_root','adapter','corpus_hash','commitment_algorithm','inventory_snapshot','manifest_digest')ORDER BY key")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        metadata.push(row?);
    }
    Ok(compute_merkle_root(&domains, &metadata))
}

fn aggregate(table: &str, leaves: &BTreeMap<String, String>) -> String {
    aggregate_domain_digest(table, leaves)
}

fn leaf_owner<'a>(key: &'a str, prefix: &str) -> Result<std::borrow::Cow<'a, str>> {
    let encoded = key
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(']'))
        .context("invalid commitment leaf key")?;
    Ok(match serde_json::from_str::<&str>(encoded) {
        Ok(owner) => std::borrow::Cow::Borrowed(owner),
        Err(_) => std::borrow::Cow::Owned(serde_json::from_str::<String>(encoded)?),
    })
}

fn batch_insert_commitments(
    conn: &Connection,
    table: &str,
    entries: &BTreeMap<String, String>,
) -> Result<()> {
    let chunk_size =
        (usize::try_from(conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_VARIABLE_NUMBER))? / 3)
            .min(250);
    anyhow::ensure!(
        chunk_size > 0,
        "SQLite variable limit cannot fit a commitment row"
    );
    let prefix = format!("leaf:[\"{table}\",");
    let mut paths: HashMap<String, i64> = conn
        .prepare_cached("SELECT path,path_id FROM path_dictionary")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let missing: BTreeSet<_> = entries
        .keys()
        .map(|key| leaf_owner(key, &prefix))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|owner| !paths.contains_key(owner.as_ref()))
        .collect();
    if !missing.is_empty() {
        let missing: Vec<_> = missing.into_iter().collect();
        for chunk in missing.chunks(chunk_size) {
            let sql = format!(
                "INSERT OR IGNORE INTO path_dictionary(path) VALUES{}",
                std::iter::repeat_n("(?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            conn.prepare_cached(&sql)?
                .execute(rusqlite::params_from_iter(
                    chunk.iter().map(|path| path.as_ref()),
                ))?;
        }
        paths = conn
            .prepare_cached("SELECT path,path_id FROM path_dictionary")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
    }
    let mut chunk = Vec::with_capacity(chunk_size);
    for (k, v) in entries {
        let path = leaf_owner(k, &prefix)?;
        let owner_id = *paths
            .get(path.as_ref())
            .context("missing commitment owner path")?;
        let mut digest = [0u8; 32];
        hex::decode_to_slice(v, &mut digest)?;
        chunk.push((owner_id, digest));
        if chunk.len() == chunk_size {
            insert_commitment_chunk(conn, table, &chunk)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        insert_commitment_chunk(conn, table, &chunk)?;
    }
    Ok(())
}

fn insert_commitment_chunk(
    conn: &Connection,
    table: &str,
    chunk: &[(i64, [u8; 32])],
) -> Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    let mut sql = String::from("INSERT OR REPLACE INTO domain_commitments VALUES");
    for i in 0..chunk.len() {
        if i > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?)");
    }
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() * 3);
    for (k, v) in chunk {
        params_vec.push(&table);
        params_vec.push(k);
        params_vec.push(v);
    }
    stmt.execute(rusqlite::params_from_iter(params_vec))?;
    Ok(())
}
fn decode_stored_digest(value: ValueRef<'_>) -> rusqlite::Result<String> {
    match value {
        ValueRef::Blob(bytes) => Ok(hex::encode(bytes)),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            }),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            "digest".into(),
            other.data_type(),
        )),
    }
}

fn stored(conn: &Connection, table: &str) -> Result<BTreeMap<String, String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.path,d.digest FROM domain_commitments d LEFT JOIN path_dictionary p ON p.path_id=d.owner_id WHERE d.domain=?1 AND d.owner_id!=0",
    )?;
    let rows = stmt
        .query_map([table], |r| {
            let owner: String = r.get(0)?;
            Ok((key(table, &owner), decode_stored_digest(r.get_ref(1)?)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    Ok(rows)
}
/// Recomputes every owner leaf and stores the deterministic output root.
///
/// # Errors
///
/// Fails if indexed rows or dictionary references are malformed, or SQLite fails.
pub fn seal(conn: &Connection) -> Result<String> {
    seal_owners(conn, None)
}

/// Reseals selected owner leaves and recomputes their domain and output roots.
///
/// `None` seals the complete index. The caller owns the SQLite transaction and
/// must keep the source rows stable until the seal has completed.
///
/// # Errors
///
/// Fails on malformed graph references, hashing errors, or SQLite failures.
pub fn seal_owners(conn: &Connection, owners: Option<&BTreeSet<String>>) -> Result<String> {
    seal_owners_with_dirty_fts(
        conn,
        owners,
        FtsDirtyDomains {
            node_search: true,
            doc_search: true,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FtsDirtyDomains {
    pub(crate) node_search: bool,
    pub(crate) doc_search: bool,
}

pub(crate) fn seal_owners_with_dirty_fts(
    conn: &Connection,
    owners: Option<&BTreeSet<String>>,
    dirty_fts: FtsDirtyDomains,
) -> Result<String> {
    conn.execute(
        "INSERT OR REPLACE INTO metadata VALUES('commitment_algorithm',?1)",
        [ALGORITHM],
    )?;
    let encoded_owners = owners.map(serde_json::to_string).transpose()?;
    let mut existing = BTreeMap::<String, BTreeMap<String, String>>::new();
    if owners.is_some() {
        let mut stmt = conn.prepare("SELECT d.domain,p.path,d.digest FROM domain_commitments d JOIN path_dictionary p ON p.path_id=d.owner_id WHERE d.owner_id!=0")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                decode_stored_digest(row.get_ref(2)?)?,
            ))
        })?;
        for row in rows {
            let (domain, owner, digest) = row?;
            existing
                .entry(domain.clone())
                .or_default()
                .insert(key(&domain, &owner), digest);
        }
    }
    for table in DOMAINS {
        let changed = leaves(conn, table, owners)?;
        if let Some(encoded) = &encoded_owners {
            conn.execute(
                "DELETE FROM domain_commitments WHERE domain=?1 AND owner_id IN(SELECT path_id FROM path_dictionary WHERE path IN(SELECT value FROM json_each(?2)))",
                params![table, encoded],
            )?;
        } else {
            conn.execute(
                "DELETE FROM domain_commitments WHERE domain=?1 AND owner_id!=0",
                [table],
            )?;
        }
        batch_insert_commitments(conn, table, &changed)?;
        let digest = if owners.is_none() {
            aggregate(table, &changed)
        } else {
            let entries = existing.entry((*table).to_owned()).or_default();
            for owner in owners.into_iter().flatten() {
                entries.remove(&key(table, owner));
            }
            entries.extend(changed);
            aggregate(table, entries)
        };
        conn.prepare_cached("INSERT OR REPLACE INTO domain_commitments VALUES(?1,0,unhex(?2))")?
            .execute(params![table, digest])?;
    }
    for &(table, order_by) in FTS {
        let clean = owners.is_some()
            && match table {
                "node_search_postings" => !dirty_fts.node_search,
                "doc_search_postings" => !dirty_fts.doc_search,
                _ => false,
            };
        if clean && conn.query_row("SELECT EXISTS(SELECT 1 FROM domain_commitments WHERE domain=?1 AND owner_id=0 AND typeof(digest)='blob' AND length(digest)=32)", [table], |r| r.get::<_, bool>(0))? {
            continue;
        }
        conn.execute(
            "INSERT OR REPLACE INTO domain_commitments VALUES(?1,0,unhex(?2))",
            params![table, fts_digest(conn, table, order_by)?],
        )?;
    }
    let root = root(conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO metadata VALUES('output_root',?1)",
        [&root],
    )?;
    Ok(root)
}

pub(crate) fn verify_delta_seal(conn: &Connection, expected_root: &str) -> Result<()> {
    let algorithm: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='commitment_algorithm'",
        [],
        |r| r.get(0),
    )?;
    let stored_root: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='output_root'",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        algorithm == ALGORITHM && stored_root == expected_root,
        "delta seal metadata mismatch"
    );
    let mut stmt = conn.prepare("SELECT domain FROM domain_commitments WHERE owner_id=0 AND typeof(digest)='blob' AND length(digest)=32")?;
    let domains: BTreeSet<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    anyhow::ensure!(
        domains.len() == DOMAINS.len() + FTS.len()
            && DOMAINS.iter().all(|d| domains.contains(*d))
            && FTS.iter().all(|(d, _)| domains.contains(*d)),
        "delta seal domains missing or invalid"
    );
    anyhow::ensure!(root(conn)? == expected_root, "delta seal root mismatch");
    Ok(())
}
/// Verifies every stored owner leaf, domain root, and output root.
///
/// # Errors
///
/// Returns a mismatch error when the committed index differs from its seal.
pub fn verify(conn: &Connection) -> Result<()> {
    verify_owners(conn, None)
}

/// Verifies selected owner leaves and all affected domain and output roots.
///
/// `None` verifies the complete index.
///
/// # Errors
///
/// Returns a mismatch error when the stored commitment differs from source rows.
pub fn verify_owners(conn: &Connection, owners: Option<&BTreeSet<String>>) -> Result<()> {
    let algorithm: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='commitment_algorithm'",
        [],
        |r| r.get(0),
    )?;
    if algorithm != ALGORITHM {
        bail!("incompatible commitment algorithm; rebuild index");
    }
    for table in DOMAINS {
        let actual = leaves(conn, table, owners)?;
        let all = stored(conn, table)?;
        let expected = if let Some(owners) = owners {
            let keys: BTreeSet<_> = owners.iter().map(|o| key(table, o)).collect();
            all.iter()
                .filter(|(k, _)| keys.contains(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        } else {
            all.clone()
        };
        if actual != expected {
            bail!("commitment mismatch: {table}");
        }
        let stored: String = conn.query_row(
            "SELECT CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END FROM domain_commitments WHERE domain=?1 AND owner_id=0",
            [table],
            |r| r.get(0),
        )?;
        if aggregate(table, &all) != stored {
            bail!("domain root mismatch: {table}");
        }
    }
    for &(table, order_by) in FTS {
        let stored: String = conn.query_row(
            "SELECT CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END FROM domain_commitments WHERE domain=?1 AND owner_id=0",
            [table],
            |r| r.get(0),
        )?;
        if fts_digest(conn, table, order_by)? != stored {
            bail!("FTS commitment mismatch: {table}");
        }
    }
    let stored: String = conn.query_row(
        "SELECT value FROM metadata WHERE key='output_root'",
        [],
        |r| r.get(0),
    )?;
    if root(conn)? != stored {
        bail!("output root mismatch");
    }
    Ok(())
}
