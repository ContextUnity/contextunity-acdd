use super::base::{aggregate, batch_insert_commitments, root, write_metadata, MetadataKey, FTS};
use super::fts5_fastpath::fts_digest;
use crate::commitments::{ALGORITHM, DOMAINS};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

#[derive(Default)]
pub(crate) struct SnapshotTimings {
    pub(crate) logical_domains_ms: f64,
    pub(crate) install_ms: f64,
}

/// Computes the FTS commitment digests independently from the logical table leaves.
pub(crate) fn fts_commitment_digests(conn: &Connection) -> Result<BTreeMap<&'static str, String>> {
    anyhow::ensure!(
        conn.is_autocommit(),
        "FTS sealing requires committed indexes"
    );
    let workers = rayon::current_num_threads().min(FTS.len()).min(4);
    let candidate = conn.path().filter(|path| !path.is_empty());
    let large: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes LIMIT 1 OFFSET 8191)",
        [],
        |row| row.get(0),
    )?;
    if let Some(candidate) = candidate.filter(|_| workers >= 2 && large) {
        // The cold writer owns this private candidate and keeps it unchanged
        // until the entire scope has joined, before installing or publishing.
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|worker| {
                    scope.spawn(move || -> Result<Vec<_>> {
                        let reader =
                            crate::db::reader::open_raw_confined_readonly(Path::new(candidate))?;
                        // Keep adjacent posting/config pairs together with two
                        // workers, so the two posting streams run independently.
                        FTS[worker * FTS.len() / workers..(worker + 1) * FTS.len() / workers]
                            .iter()
                            .map(|&(table, order_by)| {
                                Ok((table, fts_domain_digest(&reader, table, order_by)?))
                            })
                            .collect()
                    })
                })
                .collect();
            // Join every worker even if an earlier worker fails or panics.
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("FTS digest worker panicked"))?
                })
                .collect::<Vec<_>>()
        });
        let mut digests = BTreeMap::new();
        for result in results {
            digests.extend(result?);
        }
        return Ok(digests);
    }
    let mut digests = BTreeMap::new();
    for &(table, order_by) in FTS {
        digests.insert(table, fts_domain_digest(conn, table, order_by)?);
    }
    Ok(digests)
}

fn fts_domain_digest(conn: &Connection, table: &str, order_by: &str) -> Result<String> {
    fts_digest(conn, table, order_by)
}

/// Seal logical owner leaves produced from the post-linking in-memory projection.
/// FTS commitments remain sourced from their SQLite virtual tables.
pub(crate) fn seal_snapshot_from_projection(
    conn: &Connection,
    mut logical: BTreeMap<&'static str, BTreeMap<String, String>>,
    mut fts_digests: BTreeMap<&'static str, String>,
) -> Result<(String, SnapshotTimings)> {
    anyhow::ensure!(
        conn.is_autocommit(),
        "snapshot sealing requires committed ingestion"
    );
    let mut domains = Vec::with_capacity(DOMAINS.len() + FTS.len());
    let logical_started = Instant::now();
    for &table in DOMAINS {
        let entries = logical
            .remove(table)
            .with_context(|| format!("in-memory projection is missing domain {table}"))?;
        domains.push(DomainCommitment {
            table,
            digest: aggregate(table, &entries),
            entries: Some(entries),
        });
    }
    let logical_domains_ms = logical_started.elapsed().as_secs_f64() * 1000.;
    anyhow::ensure!(
        logical.is_empty(),
        "in-memory projection contains an unknown logical domain"
    );
    for &(table, _) in FTS {
        let digest = fts_digests
            .remove(table)
            .with_context(|| format!("precomputed FTS commitments are missing domain {table}"))?;
        domains.push(DomainCommitment {
            table,
            entries: None,
            digest,
        });
    }
    anyhow::ensure!(
        fts_digests.is_empty(),
        "precomputed FTS commitments contain an unknown domain"
    );
    let install_started = Instant::now();
    let root = install_snapshot(conn, SnapshotCommitments { domains })?;
    let install_ms = install_started.elapsed().as_secs_f64() * 1000.;
    Ok((
        root,
        SnapshotTimings {
            logical_domains_ms,
            install_ms,
        },
    ))
}

struct DomainCommitment {
    table: &'static str,
    entries: Option<BTreeMap<String, String>>,
    digest: String,
}

struct SnapshotCommitments {
    domains: Vec<DomainCommitment>,
}

fn install_snapshot(conn: &Connection, snapshot: SnapshotCommitments) -> Result<String> {
    anyhow::ensure!(
        conn.is_autocommit(),
        "snapshot installation requires committed indexes"
    );
    let tx = conn.unchecked_transaction()?;
    write_metadata(&tx, MetadataKey::CommitmentAlgorithm, ALGORITHM)?;
    tx.execute("DELETE FROM domain_commitments", [])?;
    for DomainCommitment {
        table,
        entries,
        digest,
    } in snapshot.domains
    {
        if let Some(entries) = entries {
            batch_insert_commitments(&tx, table, &entries)?;
        }
        tx.execute(
            "INSERT INTO domain_commitments VALUES(?1,0,unhex(?2))",
            params![table, digest],
        )?;
    }
    let root = root(&tx)?;
    write_metadata(&tx, MetadataKey::OutputRoot, &root)?;
    tx.commit()?;
    Ok(root)
}
