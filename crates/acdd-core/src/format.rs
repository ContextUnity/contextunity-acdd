//! Deterministic hashing and formatting primitives for ContextUnity ACDD.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash64_contract_matrix() {
        for (input, expected) in [
            ("", 0xcbf29ce484222325_u64),
            ("a", 0xaf63dc4c8601ec8c),
            ("hello", 0xa430d84680aabd0b),
        ] {
            for hash in [stable_hash64, crate::models::stable_hash64] {
                assert_eq!(hash(input) as u64, expected, "{input:?}");
            }
        }
    }
}
