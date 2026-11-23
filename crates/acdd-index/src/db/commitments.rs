use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet};

mod base;
mod coverage_leaves;
mod domain_leaves;
mod fts5_fastpath;
#[cfg(test)]
pub(crate) use fts5_fastpath::fts_digest_fastpath;
mod snapshot;
#[cfg(test)]
mod tests;

use crate::commitments::{CommitmentDomain, FtsDomain};
pub use crate::commitments::{hash, ALGORITHM, DOMAINS};
#[cfg(test)]
pub(crate) use base::fts_digest_vocab;
pub use base::{
    key, read_metadata, read_metadata_optional, write_metadata, MetadataKey,
    MAX_QUERY_RESULT_BYTES, MAX_SQLITE_VALUE_BYTES,
};
pub(crate) use base::{
    encode_blob, encode_int, encode_real, encode_text, encode_text_bytes,
    new_leaf_hasher,
};
pub(crate) use domain_leaves::{hash_shared_owner_leaves, SharedOwnerKind, SharedOwnerRow};
pub(crate) use snapshot::{fts_commitment_digests, seal_snapshot_from_projection, SnapshotTimings};

use base::{aggregate, batch_insert_commitments, load_paths, root, stored, FTS};
use fts5_fastpath::fts_digest;

fn leaves(
    conn: &Connection,
    table: &str,
    owners: Option<&BTreeSet<String>>,
    paths: &hashbrown::HashMap<i64, String>,
) -> Result<BTreeMap<String, String>> {
    match CommitmentDomain::from_name(table) {
        Some(CommitmentDomain::Nodes) => return domain_leaves::leaves_nodes(conn, owners, paths),
        Some(CommitmentDomain::EdgeOccurrences) => {
            return domain_leaves::leaves_edge_occurrences(conn, owners, paths)
        }
        Some(CommitmentDomain::Edges) => return domain_leaves::leaves_edges(conn, owners, paths),
        Some(CommitmentDomain::SharedOwners) => {
            return domain_leaves::leaves_shared_owners(conn, owners, paths)
        }
        Some(CommitmentDomain::Dependencies) => {
            return domain_leaves::leaves_dependencies(conn, owners, paths)
        }
        Some(CommitmentDomain::ResolutionCoverage) => {
            if let Some(leaves) = coverage_leaves::canonical_coverage_leaves(conn, owners, paths)? {
                return Ok(leaves);
            }
        }
        Some(CommitmentDomain::CoverageOwnerLanguage) => {
            let leaves = coverage_leaves::canonical_owner_language_leaves(conn, owners, paths)?
                .context("coverage_owner_language rows require canonical text")?;
            return Ok(leaves);
        }
        Some(CommitmentDomain::CoverageLanguageCounts) => {
            let leaves = coverage_leaves::canonical_language_count_leaves(conn, owners, paths)?
                .context("coverage_language_counts rows require canonical text")?;
            return Ok(leaves);
        }
        _ => {}
    }
    domain_leaves::leaves_generic(conn, table, owners)
}

/// Recomputes every owner leaf and stores the deterministic output root.
///
/// # Errors
///
/// Fails if indexed rows or dictionary references are malformed, or SQLite fails.
#[cfg(test)]
pub(crate) fn seal(conn: &Connection) -> Result<String> {
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
#[cfg(test)]
pub(crate) fn seal_owners(conn: &Connection, owners: Option<&BTreeSet<String>>) -> Result<String> {
    seal_owners_with_dirty_fts(
        conn,
        owners,
        FtsDirtySet {
            node_search: true,
            doc_search: true,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FtsDirtySet {
    pub(crate) node_search: bool,
    pub(crate) doc_search: bool,
}

impl FtsDirtySet {
    fn is_dirty(self, domain: FtsDomain) -> bool {
        match domain {
            FtsDomain::NodeSearch => self.node_search,
            FtsDomain::DocSearch => self.doc_search,
        }
    }
}

pub(crate) fn seal_owners_with_dirty_fts(
    conn: &Connection,
    owners: Option<&BTreeSet<String>>,
    dirty_fts: FtsDirtySet,
) -> Result<String> {
    write_metadata(conn, MetadataKey::CommitmentAlgorithm, ALGORITHM)?;
    let encoded_owners = owners.map(serde_json::to_string).transpose()?;
    let mut existing = BTreeMap::<String, BTreeMap<String, String>>::new();
    if owners.is_some() {
        let mut stmt = conn.prepare("SELECT d.domain,p.path,d.digest FROM domain_commitments d JOIN path_dictionary p ON p.path_id=d.owner_id WHERE d.owner_id!=0")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                base::decode_stored_digest(row.get_ref(2)?)?,
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
    let paths = load_paths(conn)?;
    for table in DOMAINS {
        let changed = leaves(conn, table, owners, &paths)?;
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
                "node_search_postings" => !dirty_fts.is_dirty(FtsDomain::NodeSearch),
                "doc_search_postings" => !dirty_fts.is_dirty(FtsDomain::DocSearch),
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
    write_metadata(conn, MetadataKey::OutputRoot, &root)?;
    Ok(root)
}

pub(crate) fn verify_delta_seal(conn: &Connection, expected_root: &str) -> Result<()> {
    let algorithm = read_metadata(conn, MetadataKey::CommitmentAlgorithm)?;
    let stored_root = read_metadata(conn, MetadataKey::OutputRoot)?;
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
pub(crate) fn verify_owners(conn: &Connection, owners: Option<&BTreeSet<String>>) -> Result<()> {
    let algorithm = read_metadata(conn, MetadataKey::CommitmentAlgorithm)?;
    if algorithm != ALGORITHM {
        bail!("incompatible commitment algorithm; rebuild index");
    }
    let paths = load_paths(conn)?;
    for table in DOMAINS {
        let actual = leaves(conn, table, owners, &paths)?;
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
    let stored = read_metadata(conn, MetadataKey::OutputRoot)?;
    if root(conn)? != stored {
        bail!("output root mismatch");
    }
    Ok(())
}
