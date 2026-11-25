use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hard upper bound (96 KiB) on serialized response payloads.
pub const MAX_OUTPUT_BYTES: usize = 96 * 1024;
/// Maximum number of items returned in a single paged query response.
pub const MAX_PAGE_SIZE: usize = 100;
/// Lower bound (1 KiB) for configurable response byte budgets.
const MIN_OUTPUT_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Response detail for paged collections.
pub enum Detail {
    #[default]
    /// Omit heavy node details.
    Compact,
    /// Include node details within the response byte budget.
    Full,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
/// Closed symbol classification filter and label (`production`, `test`, `documentation`).
pub enum SymbolClassification {
    /// Production source symbols (`is_test = 0`, non-Markdown).
    Production,
    /// Test symbols and fixtures (`is_test = 1`, non-Markdown).
    Test,
    /// Indexed Markdown documentation nodes (`language = 'markdown'`).
    Documentation,
}

impl SymbolClassification {
    /// Canonical lowercase wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
            Self::Documentation => "documentation",
        }
    }

    /// Stable SQL filter discriminant (`0` = production, `1` = test, `2` = documentation).
    pub const fn sql_code(self) -> i64 {
        match self {
            Self::Production => 0,
            Self::Test => 1,
            Self::Documentation => 2,
        }
    }
}

impl std::fmt::Display for SymbolClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SymbolClassification {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim() {
            "production" => Ok(Self::Production),
            "test" => Ok(Self::Test),
            "documentation" => Ok(Self::Documentation),
            other => anyhow::bail!(
                "invalid classification '{other}'; expected one of: production, test, documentation"
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
/// Default source snippet extraction bounds for symbol inspection.
pub struct SourceContextPolicy {
    /// Whether symbol source previews are included when `show_source` is omitted.
    pub enabled_by_default: bool,
    /// Number of leading context lines included before a symbol body (0 through 20).
    pub leading_lines: usize,
    /// Default source body lines, from 1 through 500.
    pub max_body_lines: usize,
}

impl Default for SourceContextPolicy {
    fn default() -> Self {
        Self {
            enabled_by_default: false,
            leading_lines: 5,
            max_body_lines: 200,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
/// Response pagination, detail-level, and byte-budget defaults configured in `acdd.yaml`.
pub struct ResponsePolicy {
    /// Default output verbosity (`compact` or `full`).
    pub detail: Detail,
    /// Default item count per page (1 through 100).
    pub page_size: usize,
    /// Configured maximum response size in bytes (1024 through 98304).
    pub max_output_bytes: usize,
    /// Default source preview policy for symbol read/inspect responses.
    pub source_context: SourceContextPolicy,
}

impl Default for ResponsePolicy {
    fn default() -> Self {
        Self {
            detail: Detail::Compact,
            page_size: 30,
            max_output_bytes: MAX_OUTPUT_BYTES,
            source_context: SourceContextPolicy::default(),
        }
    }
}

impl ResponsePolicy {
    /// Validates `page_size`, `max_output_bytes`, and `source_context` bounds.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(&self.page_size),
            "response.page_size must be 1..=100"
        );
        anyhow::ensure!(
            (MIN_OUTPUT_BYTES..=MAX_OUTPUT_BYTES).contains(&self.max_output_bytes),
            "response.max_output_bytes must be 1024..=98304"
        );
        anyhow::ensure!(
            self.source_context.leading_lines <= 20,
            "source leading_lines must be 0..=20"
        );
        anyhow::ensure!(
            (1..=500).contains(&self.source_context.max_body_lines),
            "source max_body_lines must be 1..=500"
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
/// Resolved pagination, detail verbosity, and generation cursor bounds for a query.
pub struct QueryOptions {
    /// Maximum number of items requested for this page (`1..=100`).
    pub limit: usize,
    /// Zero-based item offset within the ordered query result.
    pub offset: usize,
    /// Selected response detail verbosity (`Compact` or `Full`).
    pub detail: Detail,
    /// Generation token from a preceding page when `offset > 0`.
    pub generation: Option<String>,
}

impl QueryOptions {
    /// Validates and resolves caller pagination parameters against `policy` defaults.
    pub fn resolve(
        policy: &ResponsePolicy,
        limit: Option<usize>,
        offset: usize,
        detail: Option<Detail>,
        generation: Option<String>,
    ) -> anyhow::Result<Self> {
        let limit = limit.unwrap_or(policy.page_size);
        anyhow::ensure!(
            (1..=MAX_PAGE_SIZE).contains(&limit),
            "limit must be 1..=100"
        );
        anyhow::ensure!(
            offset
                .checked_add(limit)
                .is_some_and(|end| end <= i64::MAX as usize),
            "offset is too large"
        );
        anyhow::ensure!(
            offset == 0 || generation.is_some(),
            "continuation requires generation from the previous page"
        );
        Ok(Self {
            limit,
            offset,
            detail: detail.unwrap_or(policy.detail),
            generation,
        })
    }
}

/// Controls whether symbol queries include paged resolution coverage.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoverageOptions {
    /// Include paged resolution coverage entries alongside the compact summary.
    pub include_coverage: bool,
}

#[derive(Debug, Clone, Copy)]
/// Resolved source snippet extraction and pagination bounds for symbol previews.
pub struct SourceOptions {
    /// Whether source text and preview metadata should be attached to the response.
    pub enabled: bool,
    /// Number of context lines included before the symbol start line (`0..=20`).
    pub leading_lines: usize,
    /// Maximum symbol body lines included in one preview slice (`1..=500`).
    pub max_body_lines: usize,
    /// Zero-based body line offset for continuing truncated previews.
    pub offset: usize,
}

impl SourceOptions {
    /// Validates and resolves caller source-preview bounds against `policy.source_context`.
    pub fn resolve(
        policy: &ResponsePolicy,
        enabled: Option<bool>,
        leading_lines: Option<usize>,
        max_body_lines: Option<usize>,
        offset: usize,
    ) -> anyhow::Result<Self> {
        let defaults = &policy.source_context;
        let leading_lines = leading_lines.unwrap_or(defaults.leading_lines).min(20);
        let max_body_lines = max_body_lines
            .unwrap_or(defaults.max_body_lines)
            .clamp(1, 500);
        anyhow::ensure!(
            offset.checked_add(max_body_lines).is_some(),
            "source_offset is too large"
        );
        Ok(Self {
            enabled: enabled.unwrap_or(defaults.enabled_by_default),
            leading_lines,
            max_body_lines,
            offset,
        })
    }

    /// Configures a complete-symbol request whose response layer applies the byte bound.
    pub fn resolve_full_symbol(
        policy: &ResponsePolicy,
        leading_lines: Option<usize>,
        max_body_lines: Option<usize>,
        offset: usize,
    ) -> anyhow::Result<Self> {
        let defaults = &policy.source_context;
        let leading_lines = leading_lines.unwrap_or(defaults.leading_lines).min(20);
        let max_body_lines = max_body_lines
            .unwrap_or_else(|| usize::MAX.saturating_sub(offset))
            .max(1);
        anyhow::ensure!(
            offset.checked_add(max_body_lines).is_some(),
            "source_offset is too large"
        );
        Ok(Self {
            enabled: true,
            leading_lines,
            max_body_lines,
            offset,
        })
    }

    /// Returns `true` when paging from `self.offset` has a valid anchor (`offset == 0` or `generation.is_some()`).
    pub fn has_valid_paging_anchor(&self, generation: Option<&str>) -> bool {
        self.offset == 0 || generation.is_some()
    }
}

#[cfg(test)]
#[test]
fn source_preview_line_budgets_are_clamped() {
    let mut failures = Vec::new();
    let policy = ResponsePolicy::default();
    for (requested, expected) in [
        (None, 200),
        (Some(0), 1),
        (Some(1), 1),
        (Some(200), 200),
        (Some(500), 500),
        (Some(501), 500),
        (Some(usize::MAX), 500),
    ] {
        match SourceOptions::resolve(&policy, Some(true), Some(30), requested, 0) {
            Ok(options) => {
                assert_eq!(options.leading_lines, 20);
                if options.max_body_lines != expected {
                    failures.push(format!(
                        "requested {requested:?}: expected {expected} body lines, got {}",
                        options.max_body_lines
                    ));
                }
            }
            Err(error) => failures.push(format!("requested {requested:?}: {error}")),
        }
    }
    for (configured, accepted) in [
        (0, false),
        (1, true),
        (200, true),
        (500, true),
        (501, false),
    ] {
        let mut policy = policy.clone();
        policy.source_context.max_body_lines = configured;
        if policy.validate().is_ok() != accepted {
            failures.push(format!(
                "policy body lines {configured}: expected accepted={accepted}"
            ));
        }
    }
    for (requested, offset, expected) in [
        (None, 0, usize::MAX),
        (None, 35, usize::MAX - 35),
        (Some(501), 0, 501),
        (Some(35), 0, 35),
    ] {
        match SourceOptions::resolve_full_symbol(&policy, None, requested, offset) {
            Ok(options) if options.max_body_lines == expected => {}
            Ok(options) => failures.push(format!(
                "full-symbol request {requested:?} at {offset}: expected {expected}, got {}",
                options.max_body_lines
            )),
            Err(error) => failures.push(format!(
                "full-symbol request {requested:?} at {offset}: {error}"
            )),
        }
    }
    if SourceOptions::resolve_full_symbol(&policy, None, Some(usize::MAX), 1).is_ok() {
        failures.push("overflowing full-symbol source offset was accepted".to_owned());
    }
    let at_zero = SourceOptions::resolve(&policy, Some(true), None, None, 0).unwrap();
    assert!(at_zero.has_valid_paging_anchor(None));
    assert!(at_zero.has_valid_paging_anchor(Some("gen-1")));
    let at_nonzero = SourceOptions::resolve(&policy, Some(true), None, None, 10).unwrap();
    assert!(!at_nonzero.has_valid_paging_anchor(None));
    assert!(at_nonzero.has_valid_paging_anchor(Some("gen-1")));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
