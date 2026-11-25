//! Deterministic hashing, canonical JSON, byte estimation, and UTF-8 boundary primitives.

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Write};

struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Zero-allocation [`std::io::Write`] sink that counts serialized bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JsonByteCounter(pub usize);

impl Write for JsonByteCounter {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Computes the deterministic FNV-1a 64-bit hash used for indexed identifiers.
#[inline]
pub fn stable_hash64(value: &str) -> i64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        }) as i64
}

/// Computes the raw 32-byte SHA-256 digest of `data`.
#[inline]
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Computes the lowercase hexadecimal SHA-256 digest of `data`.
#[inline]
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256_bytes(data))
}

/// Streams `reader` into a SHA-256 digest without buffering the entire input in memory.
pub fn sha256_reader<R: io::Read>(reader: &mut R) -> io::Result<[u8; 32]> {
    let mut writer = Sha256Writer(Sha256::new());
    io::copy(reader, &mut writer)?;
    Ok(writer.0.finalize().into())
}

fn sha256_serde_json<T: Serialize + ?Sized>(val: &T) -> anyhow::Result<[u8; 32]> {
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, val)?;
    Ok(writer.0.finalize().into())
}

/// Streams `val` directly as compact JSON into a lowercase hexadecimal SHA-256 digest.
pub fn sha256_serde_json_hex<T: Serialize + ?Sized>(val: &T) -> anyhow::Result<String> {
    Ok(hex::encode(sha256_serde_json(val)?))
}

fn write_canonical_json_io<W: Write>(value: &Value, out: &mut W) -> io::Result<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_writer(&mut *out, value).map_err(io::Error::other)
        }
        Value::Array(items) => {
            out.write_all(b"[")?;
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.write_all(b",")?;
                }
                write_canonical_json_io(item, out)?;
            }
            out.write_all(b"]")
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|(a, _)| *a);
            out.write_all(b"{")?;
            for (idx, (key, val)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(&mut *out, key).map_err(io::Error::other)?;
                out.write_all(b":")?;
                write_canonical_json_io(val, out)?;
            }
            out.write_all(b"}")
        }
    }
}

/// Serializes `value` as compact canonical JSON with recursively sorted object keys.
pub fn canonical_json_string(value: &Value) -> String {
    let mut buf = Vec::new();
    let _ = write_canonical_json_io(value, &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

/// Computes the lowercase hexadecimal SHA-256 digest of `value`'s canonical JSON form without allocating a string.
pub fn canonical_json_sha256(value: &Value) -> String {
    let mut writer = Sha256Writer(Sha256::new());
    let _ = write_canonical_json_io(value, &mut writer);
    let digest: [u8; 32] = writer.0.finalize().into();
    hex::encode(digest)
}

/// Counts the compact JSON byte length of `val` without allocating a buffer.
pub fn estimate_json_bytes<T: Serialize + ?Sized>(val: &T) -> usize {
    let mut counter = JsonByteCounter(0);
    if serde_json::to_writer(&mut counter, val).is_ok() {
        counter.0
    } else {
        0
    }
}

/// Packs scored items into a JSON lens, preserving any prior truncation and
/// counting the compact envelope and array separators against the byte limit.
pub fn pack_json_lens(
    mut envelope: Map<String, Value>,
    items: impl ExactSizeIterator<Item = (u64, Value)>,
    max_bytes: usize,
) -> (Value, usize) {
    use crate::response::{pack_knapsack_indices, KnapsackItem};

    let total = items.len();
    let already_truncated = envelope.get("truncated").and_then(Value::as_bool) == Some(true);
    envelope.entry("total").or_insert_with(|| json!(total));
    envelope.insert("truncated".into(), json!(already_truncated));
    envelope.insert("items".into(), Value::Array(Vec::new()));
    let items: Vec<_> = items.collect();
    let candidates: Vec<_> = items
        .iter()
        .map(|(score, item)| KnapsackItem {
            weight: estimate_json_bytes(item).saturating_add(1),
            score: *score,
        })
        .collect();
    let sum = candidates.iter().fold(0_usize, |sum, candidate| {
        sum.saturating_add(candidate.weight)
    });
    let complete_bytes = estimate_json_bytes(&envelope).saturating_add(sum.saturating_sub(1));
    let kept = if total == 0
        || (complete_bytes <= max_bytes && candidates.iter().all(|item| item.score > 0))
    {
        items.into_iter().map(|(_, item)| item).collect()
    } else {
        envelope.insert("truncated".into(), json!(true));
        let mut capacity = max_bytes
            .checked_sub(estimate_json_bytes(&envelope))
            .map_or(0, |remaining| remaining.saturating_add(1));
        // A complete lens must fit with its longer `false` flag.
        if !already_truncated {
            capacity = capacity.min(sum.saturating_sub(1));
        }
        let mut selected = pack_knapsack_indices(&candidates, capacity)
            .into_iter()
            .peekable();
        items
            .into_iter()
            .enumerate()
            .filter_map(|(index, (_, item))| {
                (selected.peek() == Some(&index)).then(|| {
                    selected.next();
                    item
                })
            })
            .collect()
    };
    envelope.insert("items".into(), Value::Array(kept));
    let packed = Value::Object(envelope);
    let packed_bytes = estimate_json_bytes(&packed);
    (packed, packed_bytes)
}

/// Returns the largest byte index `<= max_bytes` (and `<= s.len()`) on a valid UTF-8 boundary.
#[inline]
pub fn utf8_floor_boundary(s: &str, max_bytes: usize) -> usize {
    let mut end = s.len().min(max_bytes);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Returns the trailing slice of `s` of at most `max_bytes` bytes aligned to a UTF-8 character boundary.
#[inline]
pub fn utf8_tail(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    if max_bytes == 0 {
        return "";
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stable_hash64_and_format_primitives_matrix() {
        for (input, expected) in [
            ("", 0xcbf29ce484222325_u64),
            ("a", 0xaf63dc4c8601ec8c),
            ("hello", 0xa430d84680aabd0b),
        ] {
            assert_eq!(stable_hash64(input) as u64, expected, "{input:?}");
        }

        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex::encode(sha256_bytes(b"abc")), sha256_hex(b"abc"));

        let sample = json!({"b": [2, 1], "a": {"y": true, "x": "ok"}});
        let streamed_hex = sha256_serde_json_hex(&sample).unwrap();
        let expected_vec = serde_json::to_vec(&sample).unwrap();
        assert_eq!(streamed_hex, sha256_hex(&expected_vec));
        assert_eq!(estimate_json_bytes(&sample), expected_vec.len());

        let reordered = json!({"a": {"x": "ok", "y": true}, "b": [2, 1]});
        assert_eq!(
            canonical_json_string(&sample),
            r#"{"a":{"x":"ok","y":true},"b":[2,1]}"#
        );
        assert_eq!(
            canonical_json_string(&sample),
            canonical_json_string(&reordered)
        );
        assert_eq!(
            canonical_json_sha256(&sample),
            canonical_json_sha256(&reordered)
        );
        assert_eq!(
            canonical_json_sha256(&sample),
            sha256_hex(canonical_json_string(&sample).as_bytes())
        );

        let utf8 = "a🦀b";
        for (max_bytes, expected_floor, expected_tail) in [
            (0, 0, ""),
            (1, 1, "b"),
            (2, 1, "b"),
            (4, 1, "b"),
            (5, 5, "🦀b"),
            (6, 6, "a🦀b"),
            (16, 6, "a🦀b"),
        ] {
            assert_eq!(
                utf8_floor_boundary(utf8, max_bytes),
                expected_floor,
                "floor {max_bytes}"
            );
            assert_eq!(
                utf8_tail(utf8, max_bytes),
                expected_tail,
                "tail {max_bytes}"
            );
        }
    }
}
