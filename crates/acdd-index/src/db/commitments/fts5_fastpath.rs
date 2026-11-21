//! Pure Rust decoder for SQLite FTS5 segment postings.

use super::*;

const FTS5_STRUCTURE_ROWID: i64 = 10;
const FTS5_STRUCTURE_V2: &[u8; 4] = b"\xff\x00\x00\x01";
const FTS5_MAX_SEGMENT: u32 = 2000;

fn fts5_segment_rowid(segid: i64, pgno: i64) -> Option<i64> {
    if !(1..=65535).contains(&segid) || !(0..(1i64 << 31)).contains(&pgno) {
        return None;
    }
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
    Some((u32::try_from(v).ok()?, n))
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
                segments.push((
                    i64::from(segid),
                    i64::from(pgno_first),
                    i64::from(pgno_last),
                ));
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
        let end = self.off.checked_add(n_new)?;
        if n_new == 0 || n_keep > self.term.len() || end > sz {
            return None;
        }
        self.term.truncate(n_keep);
        self.term.extend_from_slice(&p[self.off..end]);
        self.off = end;
        if self.pgidx >= p.len() {
            self.eod = p.len() + 1;
        } else {
            self.eod = self
                .eod
                .checked_add(usize::try_from(next_v32(p, &mut self.pgidx)?).ok()?)?;
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

struct Fts5DecodeState<'a> {
    multi_segment: bool,
    seen_term_docs: hashbrown::HashSet<(u32, i64)>,
    term_map: HashMap<std::sync::Arc<[u8]>, u32>,
    terms: Vec<std::sync::Arc<[u8]>>,
    last_term_id: Option<u32>,
    documents: &'a mut FtsDocumentMap,
    column_sort_keys: &'a [u8],
    num_columns: usize,
    postings: Vec<FtsPosting>,
}

fn decode_fts5_segment_pages(pages: &[Vec<u8>], state: &mut Fts5DecodeState<'_>) -> Option<()> {
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

        if state
            .last_term_id
            .is_none_or(|id| state.terms[id as usize].as_ref() != vocab_term)
        {
            state.last_term_id = Some(match state.term_map.get(vocab_term) {
                Some(&id) => id,
                None => {
                    let id = u32::try_from(state.terms.len()).ok()?;
                    let owned = std::sync::Arc::<[u8]>::from(vocab_term);
                    state.term_map.insert(owned.clone(), id);
                    state.terms.push(owned);
                    id
                }
            });
        }
        let term_id = state.last_term_id?;

        let is_newest_for_term_doc =
            !state.multi_segment || state.seen_term_docs.insert((term_id, cursor.rowid));
        if is_newest_for_term_doc && !b_del {
            let document = state.documents.get_mut(&cursor.rowid)?;
            let identity_sort_key = document.identity_sort_key?;

            let mut pos_off = 0usize;
            let (mut cur_col, mut cur_off) = (0u32, 0u32);
            while pos_off < poslist.len() {
                let val = next_v32(&poslist, &mut pos_off)?;
                if val == 0 {
                    return None;
                }
                if val == 1 {
                    cur_col = next_v32(&poslist, &mut pos_off)?;
                    cur_off = next_v32(&poslist, &mut pos_off)?.checked_sub(2)?;
                } else {
                    cur_off = cur_off.checked_add(val - 2)?;
                }
                let col_idx = usize::try_from(cur_col).ok()?;
                if col_idx >= state.num_columns {
                    return None;
                }
                let offset = i64::from(cur_off);
                document.token_counts[col_idx] = document.token_counts[col_idx].checked_add(1)?;
                state.postings.push(FtsPosting {
                    term: term_id,
                    identity_sort_key,
                    column_index: u8::try_from(col_idx).ok()?,
                    column_sort_key: state.column_sort_keys[col_idx],
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
                let next = cursor.rowid.wrapping_add_unsigned(delta);
                if next <= cursor.rowid {
                    return None;
                }
                cursor.rowid = next;
                cursor.off = i_off;
            }
            (n_pos, b_del) = cursor.load_n_pos()?;
        } else {
            let mut rem = i_off - sz_leaf;
            let found_next = loop {
                if cursor.leaf + 1 >= pages.len() {
                    break false;
                }
                cursor.next_page()?;
                let np = &pages[cursor.leaf];
                let sz = Fts5SegCursor::page_sz(np)?;
                if rem >= sz - 4 {
                    rem -= sz - 4;
                    continue;
                }
                let mut landing = 4 + rem;
                if landing >= cursor.eod {
                    let (ft_off, _) = decode_fts5_varint32(np.get(sz..)?)?;
                    let n_keep = if landing == usize::try_from(ft_off).ok()? {
                        0
                    } else {
                        usize::try_from(next_v32(&np[..sz], &mut landing)?).ok()?
                    };
                    cursor.off = landing;
                    cursor.load_term(n_keep)?;
                } else {
                    let first = usize::from(u16::from_be_bytes([np[0], np[1]]));
                    let value = next_v64(&np[..sz], &mut landing)?;
                    cursor.rowid = if 4 + rem == first {
                        value as i64
                    } else {
                        if value == 0 {
                            return None;
                        }
                        let next = cursor.rowid.wrapping_add_unsigned(value);
                        if next <= cursor.rowid {
                            return None;
                        }
                        next
                    };
                    cursor.off = landing;
                }
                break true;
            };
            if !found_next {
                if rem > 0 {
                    return None;
                }
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
    let Some(commitment_table) = FtsDomain::from_name(index) else {
        return Ok(None);
    };
    let spec = fts_commitment_spec(commitment_table);
    let mut config = conn.prepare(&format!(
        "SELECT k,v FROM {index}_config WHERE k IN ('version','detail')"
    ))?;
    let mut rows = config.query([])?;
    let mut version_ok = false;
    while let Some(row) = rows.next()? {
        match (row.get::<_, String>(0)?.as_str(), row.get_ref(1)?) {
            ("version", ValueRef::Integer(4) | ValueRef::Text(b"4")) => version_ok = true,
            ("detail", ValueRef::Text(b"full")) => (),
            _ => return Ok(None),
        }
    }
    if !version_ok {
        return Ok(None);
    }
    drop(rows);
    drop(config);
    let _: Option<i64> = conn
        .query_row(
            &format!("SELECT segid FROM {index}_idx LIMIT 1"),
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(structure_blob) = conn
        .query_row(
            &format!("SELECT block FROM {index}_data WHERE id=?1"),
            [FTS5_STRUCTURE_ROWID],
            |row| match row.get_ref(0)? {
                ValueRef::Blob(b) => Ok(Some(b.to_vec())),
                _ => Ok(None),
            },
        )
        .optional()?
        .flatten()
    else {
        return Ok(None);
    };
    let Some(segments) = decode_fts5_structure_segments(&structure_blob) else {
        return Ok(None);
    };
    let (mut documents, owner_ids) = load_fts_documents(conn, &spec)?;
    if documents
        .values()
        .any(|doc| doc.identity_sort_key.is_none())
    {
        return Ok(None);
    }
    let mut column_order: Vec<usize> = (0..spec.columns.len()).collect();
    column_order.sort_unstable_by_key(|column| spec.columns[*column].as_bytes());
    let mut column_sort_keys = vec![0; spec.columns.len()];
    for (rank, column) in column_order.into_iter().enumerate() {
        column_sort_keys[column] = u8::try_from(rank)?;
    }

    let mut state = Fts5DecodeState {
        multi_segment: segments.len() > 1,
        seen_term_docs: hashbrown::HashSet::new(),
        term_map: HashMap::new(),
        terms: Vec::new(),
        last_term_id: None,
        documents: &mut documents,
        column_sort_keys: &column_sort_keys,
        num_columns: spec.columns.len(),
        postings: Vec::new(),
    };

    let mut page_stmt = conn.prepare(&format!(
        "SELECT id, block FROM {index}_data WHERE id >= ?1 AND id <= ?2 ORDER BY id ASC"
    ))?;
    for (segid, pgno_first, pgno_last) in segments {
        let (Some(first_id), Some(last_id)) = (
            fts5_segment_rowid(segid, pgno_first),
            fts5_segment_rowid(segid, pgno_last),
        ) else {
            return Ok(None);
        };
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
        if decode_fts5_segment_pages(&pages, &mut state).is_none() {
            return Ok(None);
        }
    }
    drop(page_stmt);

    let Fts5DecodeState {
        terms, postings, ..
    } = state;
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
