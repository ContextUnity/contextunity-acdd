use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hard upper bound (96 KiB) on serialized response payloads.
pub const MAX_OUTPUT_BYTES: usize = 96 * 1024;
/// Maximum number of items returned in a single paged query response.
pub const MAX_PAGE_SIZE: usize = 100;
/// Lower bound (1 KiB) for configurable response byte budgets.
const MIN_OUTPUT_BYTES: usize = 1024;

/// A candidate's serialized byte cost and nonnegative relevance score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnapsackItem {
    /// Serialized cost in bytes.
    pub weight: usize,
    /// Relevance contribution.
    pub score: u64,
}

/// Selects candidates within `capacity`, preserving input order and preferring
/// earlier candidates on equal scores. Large problems use rounded-up weights.
pub fn pack_knapsack_indices(items: &[KnapsackItem], capacity: usize) -> Vec<usize> {
    const MAX_CELLS: usize = 65_536;
    fn solve(
        items: &[(usize, &KnapsackItem)],
        budget: usize,
        quantum: usize,
        selected: &mut Vec<usize>,
    ) {
        if items.is_empty() || budget == 0 {
            return;
        }
        if items.len() == 1 {
            if items[0].1.weight.div_ceil(quantum) <= budget {
                selected.push(items[0].0);
            }
            return;
        }
        let width = budget + 1;
        if (items.len() + 1).saturating_mul(width) <= MAX_CELLS {
            let mut dp = vec![0_u128; (items.len() + 1) * width];
            for row in (0..items.len()).rev() {
                let (weight, score) = (items[row].1.weight.div_ceil(quantum), u128::from(items[row].1.score));
                let (next, curr) = ((row + 1) * width, row * width);
                for rem in 0..=budget {
                    let best = dp[next + rem];
                    dp[curr + rem] = if weight <= rem { best.max(score + dp[next + rem - weight]) } else { best };
                }
            }
            let mut rem = budget;
            for (row, &(index, item)) in items.iter().enumerate() {
                let (weight, next) = (item.weight.div_ceil(quantum), (row + 1) * width);
                if weight <= rem && u128::from(item.score) + dp[next + rem - weight] >= dp[next + rem] {
                    selected.push(index);
                    rem -= weight;
                }
            }
            return;
        }
        let (left, right) = items.split_at(items.len() / 2);
        let scores = |part: &[(usize, &KnapsackItem)]| {
            let mut dp = vec![(0_u128, usize::MAX); width];
            for &(index, item) in part {
                let weight = item.weight.div_ceil(quantum);
                if weight <= budget {
                    for used in (weight..=budget).rev() {
                        let (score, first) = dp[used - weight];
                        let cand = (score + u128::from(item.score), first.min(index));
                        if cand.0 > dp[used].0 || (cand.0 == dp[used].0 && cand.1 < dp[used].1) {
                            dp[used] = cand;
                        }
                    }
                }
            }
            dp
        };
        let (left_scores, right_scores) = (scores(left), scores(right));
        let split = (0..=budget)
            .max_by_key(|&n| {
                let first = left_scores[n].1.min(right_scores[budget - n].1);
                (
                    left_scores[n].0 + right_scores[budget - n].0,
                    std::cmp::Reverse(first),
                    left_scores[n].0,
                    n,
                )
            })
            .unwrap();
        drop((left_scores, right_scores));
        solve(left, split, quantum, selected);
        solve(right, budget - split, quantum, selected);
    }
    let mut selected = Vec::new();
    let candidates: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if item.score == 0 || item.weight > capacity {
                None
            } else if item.weight == 0 {
                selected.push(index);
                None
            } else {
                Some((index, item))
            }
        })
        .collect();
    if candidates
        .iter()
        .try_fold(0_usize, |sum, (_, item)| sum.checked_add(item.weight))
        .is_some_and(|sum| sum <= capacity)
    {
        selected.extend(candidates.iter().map(|(index, _)| *index));
        selected.sort_unstable();
        return selected;
    }
    if candidates.iter().all(|(_, i)| i.weight == candidates[0].1.weight) {
        let count = (capacity / candidates[0].1.weight).min(candidates.len());
        let mut ranked = candidates;
        ranked.sort_unstable_by_key(|(index, item)| (std::cmp::Reverse(item.score), *index));
        selected.extend(ranked.into_iter().take(count).map(|(index, _)| index));
        selected.sort_unstable();
        return selected;
    }
    let quantum = capacity.div_ceil(MAX_CELLS / 2 - 1).max(1);
    solve(&candidates, capacity / quantum, quantum, &mut selected);
    let (best_idx, best) = candidates
        .iter()
        .max_by_key(|(i, item)| (item.score, std::cmp::Reverse(*i)))
        .expect("nonempty");
    let packed: u128 = selected.iter().filter(|&&i| items[i].weight != 0).map(|&i| u128::from(items[i].score)).sum();
    let first = selected.iter().find(|&&i| items[i].weight != 0);
    if u128::from(best.score) > packed || (u128::from(best.score) == packed && first.is_some_and(|&i| *best_idx < i)) {
        selected.retain(|&i| items[i].weight == 0);
        selected.push(*best_idx);
    }
    selected.sort_unstable();
    selected
}

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
            max_body_lines: 100,
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
    /// Include `file://` URIs in compact Markdown output (default `false`; auto-enabled for `detail: full`).
    pub file_uris: bool,
    /// Default source preview policy for symbol read/inspect responses.
    pub source_context: SourceContextPolicy,
}

impl Default for ResponsePolicy {
    fn default() -> Self {
        Self {
            detail: Detail::Compact,
            page_size: 12,
            max_output_bytes: MAX_OUTPUT_BYTES,
            file_uris: false,
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
    assert_eq!(policy.page_size, 12);
    assert!(!policy.file_uris);
    for (requested, expected) in [
        (None, 100),
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

#[cfg(test)]
#[test]
fn knapsack_matches_boundaries_and_rank_ties() {
    let item = |weight, score| KnapsackItem { weight, score };
    assert!(pack_knapsack_indices(&[], 0).is_empty());
    assert_eq!(pack_knapsack_indices(&[item(0, 2), item(1, 9)], 0), [0]);
    assert_eq!(pack_knapsack_indices(&[item(8, 100), item(2, 4), item(2, 4)], 2), [1]);
    assert_eq!(pack_knapsack_indices(&[item(usize::MAX, u64::MAX), item(1, 2)], usize::MAX), [0]);
    let ties = [(2, 2), (4, 0), (1, 1), (1, 1), (1, 1), (4, 0), (4, 0), (4, 0)].map(|(w, s)| item(w, s));
    assert_eq!(pack_knapsack_indices(&ties, 3), [0, 2]);
    let large = [item(70_000, 5), item(40_000, 4), item(40_000, 4)];
    assert!(pack_knapsack_indices(&large, 80_000).iter().map(|&i| large[i].weight).sum::<usize>() <= 80_000);
    let mut many = vec![item(1, 1); 32_768];
    assert_eq!(pack_knapsack_indices(&many, many.len()).len(), many.len());
    many.last_mut().unwrap().weight = 2;
    assert_eq!(pack_knapsack_indices(&many, 2), [0, 1]);
}
