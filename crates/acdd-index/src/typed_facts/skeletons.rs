use crate::models::Facts;
use crate::typed_facts::binary::{BinaryDecode, BinaryEncode};

/// Binary format header for auxiliary `statement_skeletons.skeleton_blob` rows.
const STATEMENT_SKELETONS_MAGIC: &[u8] = b"ACDD-STMT-SKELETONS\0\x01";

/// Encodes per-statement literal-masked skeleton hashes for all callables with statements.
///
/// Returns `None` when no callable in `facts` has any statement fingerprints.
pub fn encode_statement_skeletons(facts: &Facts) -> Option<Vec<u8>> {
    let count = facts
        .statement_fingerprints
        .iter()
        .filter(|entry| !entry.statements.is_empty())
        .count();
    if count == 0 {
        return None;
    }
    let count_u32 = u32::try_from(count).ok()?;
    let mut out = Vec::with_capacity(STATEMENT_SKELETONS_MAGIC.len() + 4 + count * 32);
    out.extend_from_slice(STATEMENT_SKELETONS_MAGIC);
    count_u32.encode(&mut out);
    for entry in &facts.statement_fingerprints {
        if entry.statements.is_empty() {
            continue;
        }
        entry.node_id.encode(&mut out);
        let stmt_count = u32::try_from(entry.statements.len()).ok()?;
        stmt_count.encode(&mut out);
        for stmt in &entry.statements {
            stmt.skeleton_hash.encode(&mut out);
        }
    }
    Some(out)
}

/// Decodes a `statement_skeletons.skeleton_blob` payload into `(node_id, skeleton_hashes)` pairs.
///
/// Each inner `Vec<u64>` aligns 1:1 with the callable's `statement_fingerprints` order.
pub fn decode_statement_skeletons(blob: &[u8]) -> Option<Vec<(String, Vec<u64>)>> {
    let body = blob.strip_prefix(STATEMENT_SKELETONS_MAGIC)?;
    let mut cursor = 0usize;
    let count = usize::try_from(u32::decode(body, &mut cursor)?).ok()?;
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let node_id = String::decode(body, &mut cursor)?;
        let stmt_count = usize::try_from(u32::decode(body, &mut cursor)?).ok()?;
        let remaining = body.len().checked_sub(cursor)?;
        if stmt_count > remaining / std::mem::size_of::<u64>() {
            return None;
        }
        let mut skeletons = Vec::with_capacity(stmt_count);
        for _ in 0..stmt_count {
            skeletons.push(u64::decode(body, &mut cursor)?);
        }
        out.push((node_id, skeletons));
    }
    if cursor != body.len() {
        return None;
    }
    Some(out)
}

/// Hydrates `StatementFingerprint.skeleton_hash` fields in `facts` from a decoded skeleton blob.
pub(crate) fn hydrate_statement_skeletons(facts: &mut Facts, blob: &[u8]) -> bool {
    let Some(decoded) = decode_statement_skeletons(blob) else {
        return false;
    };
    let mut by_node: std::collections::HashMap<&str, &[u64]> =
        std::collections::HashMap::with_capacity(decoded.len());
    for (node_id, skeletons) in &decoded {
        by_node.insert(node_id.as_str(), skeletons.as_slice());
    }
    for entry in &mut facts.statement_fingerprints {
        if let Some(skeletons) = by_node.get(entry.node_id.as_str()) {
            if skeletons.len() == entry.statements.len() {
                for (stmt, &skeleton_hash) in entry.statements.iter_mut().zip(skeletons.iter()) {
                    stmt.skeleton_hash = skeleton_hash;
                }
            }
        }
    }
    true
}
