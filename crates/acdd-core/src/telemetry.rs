//! Pure W3C Trace Context identifiers (`TraceId`, `SpanId`, `TraceParent`).

use anyhow::{bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TELEMETRY_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Returns whole milliseconds elapsed since `UNIX_EPOCH` (`0` if the system clock precedes the epoch).
pub fn unix_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn format_unix_ms_utc_iso(unix_ms: u128) -> String {
    let total_secs = (unix_ms / 1000) as i64;
    let millis = (unix_ms % 1000) as u32;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400) as u32;

    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil_from_days (shift epoch from 1970-01-01 to 0000-03-01).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Returns the current UTC wall-clock timestamp formatted as `%Y-%m-%dT%H:%M:%S.fffZ`.
pub fn now_utc_iso_ms() -> String {
    format_unix_ms_utc_iso(unix_epoch_ms())
}

fn next_entropy_digest() -> [u8; 32] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u128, |d| d.as_nanos());
    let seq = TELEMETRY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(seq.to_le_bytes());
    hasher.update(pid.to_le_bytes());
    hasher.finalize().into()
}

fn decode_lowercase_hex<const N: usize>(raw: &str, label: &str) -> anyhow::Result<[u8; N]> {
    ensure!(
        raw.len() == N * 2,
        "{label} must be {} hex characters, got {}",
        N * 2,
        raw.len()
    );
    ensure!(
        raw.bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{label} must contain only lowercase hexadecimal characters"
    );
    let mut bytes = [0u8; N];
    hex::decode_to_slice(raw, &mut bytes).map_err(|e| anyhow::anyhow!("invalid {label}: {e}"))?;
    ensure!(
        bytes.iter().any(|&b| b != 0),
        "{label} must not be all zeros"
    );
    Ok(bytes)
}

/// 128-bit W3C Trace Context trace identifier (32 lowercase hex chars, non-zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId([u8; 16]);

impl TraceId {
    /// Generates a new non-zero 128-bit `TraceId`.
    pub fn new() -> Self {
        let digest = next_entropy_digest();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        if bytes.iter().all(|&b| b == 0) {
            bytes[15] = 1;
        }
        Self(bytes)
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl FromStr for TraceId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(decode_lowercase_hex(s, "trace_id")?))
    }
}

impl Serialize for TraceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TraceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// 64-bit W3C Trace Context span identifier (16 lowercase hex chars, non-zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId([u8; 8]);

impl SpanId {
    /// Generates a new non-zero 64-bit `SpanId`.
    pub fn new() -> Self {
        let digest = next_entropy_digest();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[16..24]);
        if bytes.iter().all(|&b| b == 0) {
            bytes[7] = 1;
        }
        Self(bytes)
    }
}

impl Default for SpanId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl FromStr for SpanId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(decode_lowercase_hex(s, "span_id")?))
    }
}

impl Serialize for SpanId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SpanId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// W3C `traceparent` header value (`00-<trace_id>-<span_id>-01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceParent {
    /// W3C traceparent version (must not be `0xff`).
    pub version: u8,
    /// 128-bit trace identifier.
    pub trace_id: TraceId,
    /// 64-bit parent span identifier.
    pub span_id: SpanId,
    /// 8-bit trace flags (defaults to `0x01` sampled).
    pub trace_flags: u8,
}

impl TraceParent {
    /// Creates a sampled W3C `traceparent` (`version = 0x00`, `trace_flags = 0x01`).
    pub fn new(trace_id: TraceId, span_id: SpanId) -> Self {
        Self {
            version: 0x00,
            trace_id,
            span_id,
            trace_flags: 0x01,
        }
    }

    /// Parses a W3C `traceparent` string (`<version>-<trace_id>-<span_id>-<flags>`).
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        raw.parse()
    }
}

impl fmt::Display for TraceParent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}-{}-{}-{:02x}",
            self.version, self.trace_id, self.span_id, self.trace_flags
        )
    }
}

impl FromStr for TraceParent {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 4 {
            bail!("traceparent must have 4 dash-separated segments");
        }
        let version = decode_hex_byte(parts[0], "traceparent version")?;
        ensure!(version != 0xff, "traceparent version 0xff is invalid");
        let trace_id = TraceId::from_str(parts[1])?;
        let span_id = SpanId::from_str(parts[2])?;
        let trace_flags = decode_hex_byte(parts[3], "traceparent flags")?;
        Ok(Self {
            version,
            trace_id,
            span_id,
            trace_flags,
        })
    }
}

fn decode_hex_byte(raw: &str, label: &str) -> anyhow::Result<u8> {
    ensure!(raw.len() == 2, "{label} must be 2 hex characters");
    ensure!(
        raw.bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{label} must contain only lowercase hexadecimal characters"
    );
    let mut out = [0u8; 1];
    hex::decode_to_slice(raw, &mut out).map_err(|e| anyhow::anyhow!("invalid {label}: {e}"))?;
    Ok(out[0])
}

impl Serialize for TraceParent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TraceParent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_parent_round_trip_and_validation_matrix() {
        let trace_id = TraceId::new();
        let span_id = SpanId::new();
        assert_ne!(trace_id, TraceId::new());
        assert_ne!(span_id, SpanId::new());

        let tp = TraceParent::new(trace_id, span_id);
        let formatted = tp.to_string();
        assert!(formatted.starts_with("00-"));
        assert!(formatted.ends_with("-01"));
        assert_eq!(formatted.len(), 55);
        assert_eq!(TraceParent::parse(&formatted).unwrap(), tp);

        let invalid_cases = [
            "",
            "00-00000000000000000000000000000000-0000000000000001-01",
            "00-00000000000000000000000000000001-0000000000000000-01",
            "ff-00000000000000000000000000000001-0000000000000001-01",
            "00-0000000000000000000000000000000A-0000000000000001-01",
            "00-1234-0000000000000001-01",
            "00-00000000000000000000000000000001-0000000000000001",
        ];
        for raw in invalid_cases {
            assert!(TraceParent::parse(raw).is_err(), "must reject {raw:?}");
        }

        for (ms, expected) in [
            (0_u128, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_700_000_000_123, "2023-11-14T22:13:20.123Z"),
            (4_107_542_400_999, "2100-03-01T00:00:00.999Z"),
        ] {
            assert_eq!(format_unix_ms_utc_iso(ms), expected, "ms={ms}");
        }
        assert!(unix_epoch_ms() > 0);
        let now_iso = now_utc_iso_ms();
        assert_eq!(now_iso.len(), 24);
        assert!(now_iso.ends_with('Z'));
    }
}
