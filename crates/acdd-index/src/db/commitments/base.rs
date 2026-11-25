use crate::commitments::{aggregate_domain_digest, compute_merkle_root, FtsDomain, ALGORITHM};
use anyhow::{bail, Context, Result};
use hashbrown::HashMap;
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum byte length of a single SQLite cell or query response payload (8 MiB).
pub const MAX_SQLITE_VALUE_BYTES: i32 = 8 * 1024 * 1024;
/// Maximum byte length of serialized query result items (8 MiB).
pub const MAX_QUERY_RESULT_BYTES: usize = MAX_SQLITE_VALUE_BYTES as usize;

/// Canonical keys stored in the SQLite `metadata` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataKey {
    OutputRoot,
    WorkspaceRoot,
    Adapter,
    AdapterVersion,
    AdapterDigest,
    AdapterLinkedWorkspaces,
    SchemaVersion,
    IndexerEngine,
    IndexSemanticsVersion,
    CommitmentAlgorithm,
    InventorySnapshot,
    CorpusHash,
    ManifestDigest,
}

impl MetadataKey {
    /// Returns the unchanged SQLite `metadata.key` wire string.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::OutputRoot => "output_root",
            Self::WorkspaceRoot => "workspace_root",
            Self::Adapter => "adapter",
            Self::AdapterVersion => "adapter_version",
            Self::AdapterDigest => "adapter_digest",
            Self::AdapterLinkedWorkspaces => "adapter_linked_workspaces",
            Self::SchemaVersion => "schema_version",
            Self::IndexerEngine => "indexer_engine",
            Self::IndexSemanticsVersion => "index_semantics_version",
            Self::CommitmentAlgorithm => "commitment_algorithm",
            Self::InventorySnapshot => "inventory_snapshot",
            Self::CorpusHash => "corpus_hash",
            Self::ManifestDigest => "manifest_digest",
        }
    }
}

/// Reads a required value from the SQLite `metadata` table by [`MetadataKey`].
pub fn read_metadata(conn: &Connection, key: MetadataKey) -> Result<String> {
    read_metadata_optional(conn, key)?
        .with_context(|| format!("missing metadata key '{}'", key.as_str()))
}

/// Reads an optional value from the SQLite `metadata` table by [`MetadataKey`].
pub fn read_metadata_optional(conn: &Connection, key: MetadataKey) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT value FROM metadata WHERE key=?1")?
        .query_row([key.as_str()], |row| row.get(0))
        .optional()?)
}

/// Inserts or replaces a value in the SQLite `metadata` table by [`MetadataKey`].
pub fn write_metadata(conn: &Connection, key: MetadataKey, value: &str) -> Result<()> {
    conn.prepare_cached("INSERT OR REPLACE INTO metadata VALUES(?1,?2)")?
        .execute(params![key.as_str(), value])?;
    Ok(())
}

pub(super) const FTS: &[(&str, &str)] = &[
    ("node_search_postings", "term,doc,col,offset"),
    ("node_search_config", "k"),
    ("doc_search_postings", "term,doc,col,offset"),
    ("doc_search_config", "k"),
];

pub(super) fn encode_row(
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

pub(super) fn shape(table: &str) -> (&'static str, &'static str, &'static str) {
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
pub(super) struct TextSpan {
    start: usize,
    end: usize,
}

#[derive(Default)]
pub(super) struct TextArena(String);

impl TextArena {
    pub(super) fn push(&mut self, text: &str) -> TextSpan {
        let start = self.0.len();
        self.0.push_str(text);
        TextSpan {
            start,
            end: self.0.len(),
        }
    }

    pub(super) fn get(&self, span: TextSpan) -> &str {
        &self.0[span.start..span.end]
    }
}

pub(super) fn load_paths(conn: &Connection) -> Result<HashMap<i64, String>> {
    Ok(conn
        .prepare_cached("SELECT path_id,path FROM path_dictionary")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
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

pub(super) fn decode_fts5_size_array(bytes: &[u8], columns: usize) -> Result<[u32; 3]> {
    let mut offset = 0;
    let mut sizes = [0; 3];
    for size in sizes.iter_mut().take(columns) {
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
        *size = value;
    }
    anyhow::ensure!(
        offset == bytes.len(),
        "FTS5 document-size vector has trailing bytes"
    );
    Ok(sizes)
}

pub(super) struct FtsCommitmentSpec {
    pub(super) docsize_table: &'static str,
    pub(super) fts_table: &'static str,
    pub(super) owner_join: &'static str,
    pub(super) identity: &'static str,
    pub(super) identity_kind: FtsIdentityKind,
    pub(super) columns: &'static [&'static str],
}

#[derive(Clone, Copy)]
pub(super) enum FtsIdentityKind {
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

pub(super) struct FtsDocument {
    pub(super) identity_sort_key: Option<i64>,
    pub(super) sizes: [u32; 3],
    pub(super) token_counts: [u32; 3],
}

pub(super) type FtsDocumentMap = HashMap<i64, FtsDocument>;
pub(super) type FtsOwnerIds = Vec<Vec<u8>>;

pub(super) struct FtsPosting {
    pub(super) term: u32,
    pub(super) identity_sort_key: i64,
    pub(super) column_index: u8,
    pub(super) column_sort_key: u8,
    pub(super) offset: i64,
}

pub(super) fn fts_commitment_spec(index: FtsDomain) -> FtsCommitmentSpec {
    match index {
        FtsDomain::NodeSearch => FtsCommitmentSpec {
            docsize_table: "node_search_docsize",
            fts_table: "node_search",
            owner_join: "LEFT JOIN nodes AS n ON n.node_hash=d.id",
            identity: "n.node_hash",
            identity_kind: FtsIdentityKind::NodeHash,
            columns: &["search_text"],
        },
        FtsDomain::DocSearch => FtsCommitmentSpec {
            docsize_table: "doc_search_docsize",
            fts_table: "doc_search",
            owner_join: "LEFT JOIN doc_sections AS s ON s.rowid=d.id",
            identity: "s.doc_id",
            identity_kind: FtsIdentityKind::DocId,
            columns: &["section_title", "content", "invariants"],
        },
    }
}

pub(super) fn load_fts_documents(
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
        let ValueRef::Blob(size_blob) = row.get_ref(2)? else {
            bail!(
                "missing active FTS5 docsize row: {}:{doc}",
                spec.docsize_table
            );
        };
        let sizes = decode_fts5_size_array(size_blob, spec.columns.len()).with_context(|| {
            format!("invalid FTS5 document sizes: {}:{doc}", spec.docsize_table)
        })?;
        documents.push((doc, identity, sizes));
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
    for (doc, identity, sizes) in documents {
        let identity_sort_key = match identity.as_ref() {
            Some(FtsRowIdentity::NodeHash(node_hash)) => Some(*node_hash),
            Some(FtsRowIdentity::DocId(doc_id)) => owner_ranks.get(doc_id).copied(),
            None => None,
        };
        by_doc.insert(
            doc,
            FtsDocument {
                identity_sort_key,
                sizes,
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
        for (column_index, column) in spec.columns.iter().enumerate() {
            anyhow::ensure!(
                document.sizes[column_index] == document.token_counts[column_index],
                "FTS5 document size mismatch: {}:{doc} column {column}",
                spec.docsize_table
            );
        }
    }
    Ok(())
}

pub(super) fn finalize_fts_postings_digest<T: AsRef<[u8]> + Ord>(
    h: &mut Sha256,
    spec: &FtsCommitmentSpec,
    documents: &FtsDocumentMap,
    owner_ids: &FtsOwnerIds,
    terms: Vec<T>,
    mut postings: Vec<FtsPosting>,
) -> Result<()> {
    let terms = if terms.windows(2).all(|w| w[0] < w[1]) {
        terms
    } else {
        let mut sorted_indexed: Vec<(u32, T)> = terms
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
                .context("sorted FTS5 posting lost its term")?
                .as_ref(),
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

pub(super) fn fts_column_sort_keys(columns: &[&str]) -> Result<Vec<u8>> {
    let mut column_order: Vec<usize> = (0..columns.len()).collect();
    column_order.sort_unstable_by_key(|column| columns[*column].as_bytes());
    let mut column_sort_keys = vec![0; columns.len()];
    for (rank, column) in column_order.into_iter().enumerate() {
        column_sort_keys[column] = u8::try_from(rank)?;
    }
    Ok(column_sort_keys)
}

/// Computes the canonical FTS digest using SQLite's instance vocabulary.
pub(crate) fn fts_digest_vocab(conn: &Connection, table: &str, order_by: &str) -> Result<String> {
    let table = match table {
        "node_search" => "node_search_postings",
        "doc_search" => "doc_search_postings",
        _ => table,
    };
    let mut h = Sha256::new();
    h.update(table);
    if let Some(index) = table.strip_suffix("_postings") {
        let vocab = ensure_fts_vocab(conn, index)?;
        let spec = fts_commitment_spec(
            FtsDomain::from_name(index)
                .with_context(|| format!("unsupported FTS commitment index: {index}"))?,
        );
        let (mut documents, owner_ids) = load_fts_documents(conn, &spec)?;
        let column_sort_keys = fts_column_sort_keys(spec.columns)?;
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

pub(super) fn root(conn: &Connection) -> Result<String> {
    let mut domains = Vec::new();
    let mut stmt = conn.prepare("SELECT domain,CASE WHEN typeof(digest)='blob' THEN lower(hex(digest)) ELSE digest END FROM domain_commitments WHERE owner_id=0 ORDER BY domain")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        domains.push(row?);
    }
    let mut metadata = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT key,value FROM metadata WHERE key IN(?1,?2,?3,?4,?5,?6,?7,?8,?9)ORDER BY key",
    )?;
    let merkle_keys = [
        MetadataKey::SchemaVersion.as_str(),
        MetadataKey::IndexSemanticsVersion.as_str(),
        MetadataKey::IndexerEngine.as_str(),
        MetadataKey::WorkspaceRoot.as_str(),
        MetadataKey::Adapter.as_str(),
        MetadataKey::CorpusHash.as_str(),
        MetadataKey::CommitmentAlgorithm.as_str(),
        MetadataKey::InventorySnapshot.as_str(),
        MetadataKey::ManifestDigest.as_str(),
    ];
    for row in stmt.query_map(merkle_keys, |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        metadata.push(row?);
    }
    Ok(compute_merkle_root(&domains, &metadata))
}

pub(super) fn aggregate(table: &str, leaves: &BTreeMap<String, String>) -> String {
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

pub(super) fn batch_insert_commitments(
    conn: &Connection,
    table: &str,
    entries: &BTreeMap<String, String>,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
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

pub(super) fn decode_stored_digest(value: ValueRef<'_>) -> rusqlite::Result<String> {
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

pub(super) fn stored(conn: &Connection, table: &str) -> Result<BTreeMap<String, String>> {
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
