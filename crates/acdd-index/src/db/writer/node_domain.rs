use crate::{
    db::commitments::{
        encode_blob, encode_int as encode_integer, encode_text, encode_text_bytes, key as leaf_key,
        new_leaf_hasher,
    },
    models::{stable_hash64, Node},
};
use anyhow::{Context, Result};
use hashbrown::HashMap;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) type OwnerHashers<'a> = HashMap<&'a str, Sha256>;
pub(super) type Leaves = BTreeMap<String, String>;

pub(super) struct NodeDomain<'a> {
    hashers: OwnerHashers<'a>,
}

impl<'a> NodeDomain<'a> {
    pub(super) fn new() -> Self {
        Self {
            hashers: OwnerHashers::new(),
        }
    }

    pub(super) fn push(&mut self, owner: &'a str, node: &Node, details: &[u8]) -> Result<()> {
        let line = i64::try_from(node.line).context("node line exceeds SQLite range")?;
        let end_line =
            i64::try_from(node.end_line).context("node end line exceeds SQLite range")?;
        let hasher = row_hasher(&mut self.hashers, "nodes", owner);
        encode_text(hasher, &node.id);
        encode_text(hasher, node.kind.as_str());
        encode_text(hasher, &node.name);
        encode_text(hasher, &node.qualname);
        encode_text(hasher, &node.path);
        encode_integer(hasher, line);
        encode_integer(hasher, end_line);
        encode_integer(hasher, i64::from(node.is_test));
        encode_text(hasher, node.language.as_str());
        encode_integer(hasher, i64::from(node.generated));
        encode_text_bytes(hasher, details);
        encode_integer(hasher, stable_hash64(&node.id));
        Ok(())
    }

    pub(super) fn finish(self) -> Result<BTreeMap<String, String>> {
        finish_hashers("nodes", self.hashers)
    }
}

pub(super) fn local_facts_leaf(
    path: &str,
    digest: &str,
    language: &str,
    is_test: bool,
    facts_blob: &[u8],
) -> Result<(String, String)> {
    let mut hasher = new_leaf_hasher("local_facts", path);
    hasher.update([0xff]);
    encode_text(&mut hasher, path);
    encode_text(&mut hasher, digest);
    encode_text(&mut hasher, language);
    encode_integer(&mut hasher, i64::from(is_test));
    encode_integer(&mut hasher, 0);
    encode_blob(&mut hasher, facts_blob);
    let key = leaf_key("local_facts", path);
    let digest = hex::encode(hasher.finalize());
    Ok((key, digest))
}

pub(super) fn row_hasher<'map, 'owner>(
    hashers: &'map mut OwnerHashers<'owner>,
    table: &str,
    owner: &'owner str,
) -> &'map mut Sha256 {
    let hasher = hashers
        .entry(owner)
        .or_insert_with(|| new_leaf_hasher(table, owner));
    hasher.update([0xff]);
    hasher
}

pub(super) fn finish_hashers(table: &'static str, hashers: OwnerHashers<'_>) -> Result<Leaves> {
    Ok(hashers
        .into_iter()
        .map(|(owner, hasher)| (leaf_key(table, owner), hex::encode(hasher.finalize())))
        .collect())
}
