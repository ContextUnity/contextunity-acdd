use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_BRACE_EXPANSIONS: usize = 64;

/// Normalized pure workspace path policy implementing the four non-conflated predicates from `ADR-0002`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePathPolicy {
    /// Active `Run` `spec_path` and `archive_path` entries managed exclusively by the state engine.
    #[serde(default)]
    pub engine_managed_paths: BTreeSet<String>,
    /// Configured normalized relative path to the state SQLite database.
    pub state_db_path: String,
    /// Configured normalized relative path to the code-map SQLite database.
    pub code_db_path: String,
    /// Optional maximum snapshot/commit blob size in bytes (`None` disables ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_snapshot_file_bytes: Option<u64>,
    /// Optional normalized lowercase binary extensions exempt from NUL-byte rejection (`None` disables NUL check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_binary_extensions: Option<BTreeSet<String>>,
}

impl Default for WorkspacePathPolicy {
    fn default() -> Self {
        Self {
            engine_managed_paths: BTreeSet::new(),
            state_db_path: ".acdd/state.sqlite".to_owned(),
            code_db_path: ".acdd/code-map.sqlite".to_owned(),
            max_snapshot_file_bytes: None,
            allowed_binary_extensions: None,
        }
    }
}

impl WorkspacePathPolicy {
    /// Construct a `WorkspacePathPolicy` from workspace configuration paths.
    ///
    /// Converts relative `state_db_rel` and `code_db_rel` cleanly, falling back to
    /// `.acdd/state.sqlite` / `.acdd/code-map.sqlite` only when blank (or non-relative).
    /// `engine_managed_paths` must contain both active `Run` `spec_path` and `archive_path` entries.
    pub fn from_workspace_config(
        state_db_rel: &str,
        code_db_rel: &str,
        engine_managed_paths: impl IntoIterator<Item = String>,
    ) -> Self {
        let managed = engine_managed_paths
            .into_iter()
            .filter_map(|p| normalize_rel_path(&p))
            .collect();
        let state_db = if state_db_rel.trim().is_empty() {
            ".acdd/state.sqlite".to_owned()
        } else {
            normalize_rel_path(state_db_rel).unwrap_or_else(|| ".acdd/state.sqlite".to_owned())
        };
        let code_db = if code_db_rel.trim().is_empty() {
            ".acdd/code-map.sqlite".to_owned()
        } else {
            normalize_rel_path(code_db_rel).unwrap_or_else(|| ".acdd/code-map.sqlite".to_owned())
        };
        Self {
            engine_managed_paths: managed,
            state_db_path: state_db,
            code_db_path: code_db,
            max_snapshot_file_bytes: None,
            allowed_binary_extensions: None,
        }
    }

    /// Construct a `WorkspacePathPolicy`, normalizing all supplied relative paths.
    pub fn new(
        engine_managed_paths: impl IntoIterator<Item = String>,
        state_db_path: impl AsRef<str>,
        code_db_path: impl AsRef<str>,
    ) -> Self {
        Self::from_workspace_config(
            state_db_path.as_ref(),
            code_db_path.as_ref(),
            engine_managed_paths,
        )
    }

    /// Configure optional snapshot/commit file size ceiling and binary extension allowlist.
    pub fn with_snapshot_limits(
        mut self,
        max_file_bytes: Option<u64>,
        allowed_binary_extensions: Option<BTreeSet<String>>,
    ) -> Self {
        self.max_snapshot_file_bytes = max_file_bytes;
        self.allowed_binary_extensions = allowed_binary_extensions.map(|exts| {
            exts.into_iter()
                .filter_map(|ext| normalize_binary_extension(&ext).ok())
                .collect()
        });
        self
    }

    /// Return `true` if `rel_path`'s extension is allowed when `allowed_binary_extensions` is enabled
    /// (returns `true` for all paths when `allowed_binary_extensions` is `None`).
    pub fn is_binary_extension_allowed(&self, rel_path: &str) -> bool {
        let Some(ref allowed) = self.allowed_binary_extensions else {
            return true;
        };
        std::path::Path::new(rel_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| allowed.contains(&ext.to_ascii_lowercase()))
    }

    /// Predicate 1: Return `true` if `rel_path` matches any active `Run`'s `spec_path` or `archive_path`.
    ///
    /// Uses APFS-safe case-folding so case variants (e.g. `DOCS/MILESTONES/AC-008.MD`) cannot bypass
    /// engine-managed spec exclusions on case-insensitive volumes.
    pub fn is_engine_managed(&self, rel_path: &str) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        if self.engine_managed_paths.contains(&norm) {
            return true;
        }
        let norm_lower = norm.to_lowercase();
        self.engine_managed_paths
            .iter()
            .any(|managed| managed.to_lowercase() == norm_lower)
    }

    /// Predicate 2: Return `true` if `rel_path` belongs in candidate/snapshot `TreeOid` calculations.
    ///
    /// Excludes `.git/**`, `.worktrees/**`, `is_engine_managed(rel_path)`, configured `state_db`/`code_db`
    /// (plus `-wal`, `-shm`, `.backup` sidecars), and `.acdd/**` (all APFS case-insensitively) EXCEPT
    /// the tracked whitelist `.acdd/profile.yaml`, `.acdd/profiles/**`, and `.acdd/frameworks/**`.
    /// Explicitly includes tracked artifacts such as `Cargo.lock` and `tests/**/*.jsonl`.
    pub fn is_candidate_included(&self, rel_path: &str) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        if is_under_dir_case_insensitive(&norm, ".git")
            || is_under_dir_case_insensitive(&norm, ".worktrees")
        {
            return false;
        }
        if self.is_engine_managed(&norm) {
            return false;
        }
        if is_db_or_sidecar(&norm, &self.state_db_path)
            || is_db_or_sidecar(&norm, &self.code_db_path)
        {
            return false;
        }
        if is_under_dir_case_insensitive(&norm, ".acdd") && !is_acdd_tracked_whitelist(&norm) {
            return false;
        }
        true
    }

    /// Predicate 3: Return `true` if `rel_path` matches at least one component-wise glob or directory entry in `scope`.
    pub fn is_scope_admitted(&self, rel_path: &str, scope: &[String]) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        let path_segs: Vec<&str> = norm.split('/').collect();
        scope
            .iter()
            .any(|pattern| pattern_matches_segments(pattern, &path_segs))
    }

    /// Return `true` if `rel_path` is protected by `protected_paths` (APFS case-insensitively)
    /// or fails closed on an invalid `rel_path` or malformed `protected_paths` glob.
    pub fn is_protected_path(&self, rel_path: &str, protected_paths: &[String]) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return true;
        };
        if protected_paths.iter().any(|p| !is_valid_scope_glob(p)) {
            return true;
        }
        if protected_paths.is_empty() {
            return false;
        }
        let lower_norm = norm.to_lowercase();
        let lower_segs: Vec<&str> = lower_norm.split('/').collect();
        protected_paths.iter().any(|pattern| {
            let lower_pat = pattern.to_lowercase();
            pattern_matches_segments(&lower_pat, &lower_segs)
        })
    }

    /// Predicate 4: Return `true` if an agent may write `rel_path` under `admitted_scope` and `protected_paths`.
    ///
    /// Forbids `.git/**`, `.worktrees/**`, `is_engine_managed`, `state_db`/`code_db` (+ sidecars),
    /// `protected_paths`, and non-whitelisted `.acdd/**` (all APFS case-insensitively); requires
    /// `is_scope_admitted(rel_path, admitted_scope)`.
    pub fn is_agent_writable(
        &self,
        rel_path: &str,
        admitted_scope: &[String],
        protected_paths: &[String],
    ) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        if !self.is_candidate_included(&norm) {
            return false;
        }
        if self.is_protected_path(&norm, protected_paths) {
            return false;
        }
        self.is_scope_admitted(&norm, admitted_scope)
    }

    /// Validate a `save.report` destination path against workspace confinement and `protected_paths`.
    ///
    /// Strictly confined to `.acdd/reports/*.{md,json}` (case-insensitive extension, APFS-safe
    /// rejection of non-canonical `.ACDD/` casing, `state_db`/`code_db` + sidecars, engine-managed
    /// paths, and `protected_paths`).
    pub fn is_valid_report_path(&self, rel_path: &str, protected_paths: &[String]) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        let under_reports =
            norm.starts_with(".acdd/reports/") && norm.len() > ".acdd/reports/".len();
        let valid_ext = has_extension_ci(&norm, "md") || has_extension_ci(&norm, "json");
        under_reports
            && valid_ext
            && !is_db_or_sidecar(&norm, &self.state_db_path)
            && !is_db_or_sidecar(&norm, &self.code_db_path)
            && !self.is_engine_managed(&norm)
            && !self.is_protected_path(&norm, protected_paths)
    }
}

/// Validate and normalize a single binary extension entry (`".PNG"` -> `"png"`).
pub fn normalize_binary_extension(raw: &str) -> Result<String, String> {
    if raw.chars().any(char::is_whitespace) {
        return Err(format!(
            "limits.allowed_binary_extensions entry '{raw}' must not contain whitespace"
        ));
    }
    let stripped = raw.strip_prefix('.').unwrap_or(raw);
    if stripped.is_empty()
        || !stripped
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_'))
    {
        return Err(format!(
            "limits.allowed_binary_extensions entry '{raw}' must be a single extension (ASCII alphanumeric, '+', '-', or '_', optional leading '.')"
        ));
    }
    Ok(stripped.to_ascii_lowercase())
}

/// Validate and normalize an `allowed_binary_extensions` list into a lowercase [`BTreeSet`].
pub fn normalize_binary_extensions<I, S>(exts: I) -> Result<BTreeSet<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    exts.into_iter()
        .map(|ext| normalize_binary_extension(ext.as_ref()))
        .collect()
}

/// Normalize a workspace-relative path purely lexically without touching the filesystem.
///
/// Rejects empty paths, NUL bytes, absolute paths, Windows drive prefixes, and `..` parent traversal.
pub fn normalize_rel_path(raw: &str) -> Option<String> {
    normalize_rel_path_exact(raw.trim())
}

/// Normalize a workspace-relative path without trimming leading/trailing whitespace on components.
///
/// Preserves literal spaces in filenames (e.g. `"src/ leading.rs"`) while rejecting empty paths,
/// NUL bytes, absolute paths, Windows drive prefixes, and `..` parent traversal.
pub(crate) fn normalize_rel_path_exact(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }
    if raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }

    let replaced = raw.replace('\\', "/");
    let mut segments = Vec::new();
    for seg in replaced.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None,
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments.join("/"))
}

/// Return `true` if `norm_path` equals `dir` or is a descendant of `dir` using ASCII case-insensitive comparison.
pub fn is_under_dir_case_insensitive(norm_path: &str, dir: &str) -> bool {
    if norm_path.eq_ignore_ascii_case(dir) {
        return true;
    }
    norm_path
        .get(..dir.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(dir))
        && norm_path.as_bytes().get(dir.len()) == Some(&b'/')
}

fn has_extension_ci(rel_path: &str, expected: &str) -> bool {
    std::path::Path::new(rel_path.trim())
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(expected))
}

/// Return `true` if `rel_path` has a `.json` file extension (case-insensitive, APFS-safe).
pub fn has_json_extension(rel_path: &str) -> bool {
    has_extension_ci(rel_path, "json")
}

fn is_db_or_sidecar(norm_path: &str, db_path: &str) -> bool {
    let norm_lower = norm_path.to_lowercase();
    let db_lower = db_path.to_lowercase();
    if norm_lower == db_lower {
        return true;
    }
    let Some(suffix) = norm_lower.strip_prefix(&db_lower) else {
        return false;
    };
    matches!(suffix, "-wal" | "-shm" | ".backup")
}

fn is_acdd_tracked_whitelist(norm_path: &str) -> bool {
    norm_path == ".acdd/profile.yaml"
        || norm_path.starts_with(".acdd/profiles/")
        || norm_path.starts_with(".acdd/frameworks/")
}

/// Validate a workspace-relative scope path or brace-glob pattern.
///
/// Enforces non-empty input, balanced non-empty `{...}` brace groups with at most
/// 64 expanded variants (`MAX_BRACE_EXPANSIONS`), and clean relative path normalization
/// (rejecting `..`, leading `/`/`\`, Windows drive prefixes, and NUL bytes) on every variant.
pub fn is_valid_scope_glob(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return false;
    }
    if !trimmed.contains('{') && !trimmed.contains('}') {
        return normalize_rel_path(trimmed).is_some();
    }
    let variants = expand_glob_braces(trimmed);
    !variants.is_empty() && variants.iter().all(|v| normalize_rel_path(v).is_some())
}

/// Expand `{a,b}` brace groups (including nested and sequential groups) iteratively.
///
/// Caps total expanded variants at 64 (`MAX_BRACE_EXPANSIONS`) and fails closed
/// (returning an empty `Vec`) on unbalanced braces, empty `{}` groups, or cap overflow.
pub fn expand_glob_braces(raw: &str) -> Vec<String> {
    if !raw.contains('{') && !raw.contains('}') {
        return vec![raw.to_owned()];
    }
    if !has_balanced_nonempty_braces(raw) {
        return Vec::new();
    }

    let mut queue = VecDeque::new();
    queue.push_back(raw.to_owned());
    let mut out = Vec::new();

    while let Some(current) = queue.pop_front() {
        let Some((open_idx, close_idx, parts)) = find_expandable_brace_group(&current) else {
            out.push(current);
            continue;
        };
        if out.len() + queue.len() + parts.len() > MAX_BRACE_EXPANSIONS {
            return Vec::new();
        }
        let prefix = &current[..open_idx];
        let suffix = &current[close_idx + 1..];
        for part in parts {
            queue.push_back(format!("{prefix}{part}{suffix}"));
        }
    }

    out
}

fn has_balanced_nonempty_braces(raw: &str) -> bool {
    let mut depth = 0usize;
    let mut prev = None;
    for ch in raw.chars() {
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth == 0 || prev == Some('{') {
                    return false;
                }
                depth -= 1;
            }
            _ => {}
        }
        prev = Some(ch);
    }
    depth == 0
}

fn find_expandable_brace_group(s: &str) -> Option<(usize, usize, Vec<&str>)> {
    let bytes = s.as_bytes();
    let mut search_start = 0usize;

    while let Some(rel_open) = s[search_start..].find('{') {
        let open_idx = search_start + rel_open;
        let mut depth = 0usize;
        let mut close_idx = None;
        let mut split_points = Vec::new();

        for (offset, ch) in s[open_idx..].char_indices() {
            let idx = open_idx + offset;
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close_idx = Some(idx);
                        break;
                    }
                }
                ',' if depth == 1 => split_points.push(idx),
                _ => {}
            }
        }

        let close_idx = close_idx?;
        if !split_points.is_empty() {
            let mut parts = Vec::with_capacity(split_points.len() + 1);
            let mut start = open_idx + 1;
            for comma_idx in split_points {
                parts.push(&s[start..comma_idx]);
                start = comma_idx + 1;
            }
            parts.push(&s[start..close_idx]);
            return Some((open_idx, close_idx, parts));
        }

        if open_idx + 1 >= bytes.len() {
            break;
        }
        search_start = open_idx + 1;
    }

    None
}

/// Return `true` if `path` matches `pattern` using normalized segments, `{a,b}` braces, and `**`/`*`/`?` wildcards.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let Some(norm_path) = normalize_rel_path(path) else {
        return false;
    };
    let path_segs: Vec<&str> = norm_path.split('/').collect();
    pattern_matches_segments(pattern, &path_segs)
}

/// Return `true` if two scope pattern lists can match at least one common normalized path.
pub fn scopes_overlap(a: &[String], b: &[String]) -> bool {
    for pat_a in a {
        for pat_b in b {
            if patterns_overlap(pat_a, pat_b) {
                return true;
            }
        }
    }
    false
}

/// Return `true` if two scope pattern lists can match a common normalized path under APFS case-folding.
pub fn scopes_overlap_case_insensitive(a: &[String], b: &[String]) -> bool {
    let lower_a: Vec<String> = a.iter().map(|p| p.to_lowercase()).collect();
    let lower_b: Vec<String> = b.iter().map(|p| p.to_lowercase()).collect();
    scopes_overlap(&lower_a, &lower_b)
}

/// Return `true` if two normalized scope patterns (literal directories or globs, including `{a,b}` braces) can match a common path.
pub fn patterns_overlap(raw_a: &str, raw_b: &str) -> bool {
    let expanded_b: Vec<Vec<String>> = expand_glob_braces(raw_b)
        .into_iter()
        .filter_map(|variant| normalize_rel_path(&variant))
        .map(|norm| expand_pattern_segments(&norm))
        .collect();
    if expanded_b.is_empty() {
        return false;
    }

    for var_a in expand_glob_braces(raw_a) {
        let Some(norm_a) = normalize_rel_path(&var_a) else {
            continue;
        };
        let segs_a = expand_pattern_segments(&norm_a);
        for segs_b in &expanded_b {
            let mut memo = BTreeMap::new();
            if match_pattern_intersection(&segs_a, segs_b, 0, 0, &mut memo) {
                return true;
            }
        }
    }
    false
}

fn expand_pattern_segments(norm: &str) -> Vec<String> {
    let mut segs: Vec<String> = norm.split('/').map(str::to_owned).collect();
    let has_wildcard = segs
        .iter()
        .any(|s| s == "**" || s.contains('*') || s.contains('?'));
    if !has_wildcard {
        // Per `WorkspacePathPolicy::is_scope_admitted`, a literal directory matches itself and descendants.
        segs.push("**".to_owned());
    }
    segs
}

fn match_pattern_intersection(
    a: &[String],
    b: &[String],
    ia: usize,
    ib: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(&cached) = memo.get(&(ia, ib)) {
        return cached;
    }
    let res = match (a.get(ia).map(String::as_str), b.get(ib).map(String::as_str)) {
        (None, None) => true,
        (Some("**"), Some("**")) => {
            match_pattern_intersection(a, b, ia + 1, ib, memo)
                || match_pattern_intersection(a, b, ia, ib + 1, memo)
        }
        (Some("**"), _) => {
            match_pattern_intersection(a, b, ia + 1, ib, memo)
                || (ib < b.len() && match_pattern_intersection(a, b, ia, ib + 1, memo))
        }
        (_, Some("**")) => {
            match_pattern_intersection(a, b, ia, ib + 1, memo)
                || (ia < a.len() && match_pattern_intersection(a, b, ia + 1, ib, memo))
        }
        (Some(sa), Some(sb)) => {
            segment_globs_intersect(sa, sb)
                && match_pattern_intersection(a, b, ia + 1, ib + 1, memo)
        }
        _ => false,
    };
    memo.insert((ia, ib), res);
    res
}

fn segment_globs_intersect(a: &str, b: &str) -> bool {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let mut memo = BTreeMap::new();
    segment_chars_intersect(&ca, &cb, 0, 0, &mut memo)
}

fn segment_chars_intersect(
    a: &[char],
    b: &[char],
    ia: usize,
    ib: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(&cached) = memo.get(&(ia, ib)) {
        return cached;
    }
    let res = match (a.get(ia).copied(), b.get(ib).copied()) {
        (None, None) => true,
        (Some('*'), _) => {
            segment_chars_intersect(a, b, ia + 1, ib, memo)
                || (ib < b.len() && segment_chars_intersect(a, b, ia, ib + 1, memo))
        }
        (_, Some('*')) => {
            segment_chars_intersect(a, b, ia, ib + 1, memo)
                || (ia < a.len() && segment_chars_intersect(a, b, ia + 1, ib, memo))
        }
        (Some('?'), Some(_)) | (Some(_), Some('?')) => {
            segment_chars_intersect(a, b, ia + 1, ib + 1, memo)
        }
        (Some(ca), Some(cb)) => ca == cb && segment_chars_intersect(a, b, ia + 1, ib + 1, memo),
        _ => false,
    };
    memo.insert((ia, ib), res);
    res
}

fn pattern_matches_segments(raw_pattern: &str, path_segs: &[&str]) -> bool {
    for variant in expand_glob_braces(raw_pattern) {
        let Some(norm_pat) = normalize_rel_path(&variant) else {
            continue;
        };
        let pat_segs: Vec<&str> = norm_pat.split('/').collect();
        let has_wildcard = pat_segs
            .iter()
            .any(|seg| *seg == "**" || seg.contains('*') || seg.contains('?'));
        let matched = if !has_wildcard {
            path_segs.len() >= pat_segs.len() && path_segs[..pat_segs.len()] == pat_segs[..]
        } else {
            match_segments(&pat_segs, path_segs)
        };
        if matched {
            return true;
        }
    }
    false
}

fn match_segments(pat: &[&str], path: &[&str]) -> bool {
    match (pat.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            if match_segments(&pat[1..], path) {
                return true;
            }
            !path.is_empty() && match_segments(pat, &path[1..])
        }
        (Some(p), Some(s)) => segment_glob_match(p, s) && match_segments(&pat[1..], &path[1..]),
        _ => false,
    }
}

fn segment_glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_policy_candidate_and_engine_managed_matrix() {
        let policy = WorkspacePathPolicy::new(
            [
                "./docs/milestones/AC-008.md".to_owned(),
                "docs/milestones/archive/AC-008.md".to_owned(),
            ],
            ".acdd/state.sqlite",
            "custom/index/code-map.sqlite",
        );

        // (path, expected_engine_managed, expected_candidate_included)
        let cases: &[(&str, bool, bool)] = &[
            ("docs/milestones/AC-008.md", true, false),
            ("DOCS/MILESTONES/AC-008.MD", true, false),
            ("docs/milestones/archive/AC-008.md", true, false),
            ("docs/milestones/AC-009.md", false, true),
            (".git/HEAD", false, false),
            (".GIT/HEAD", false, false),
            (".worktrees/foo/src/lib.rs", false, false),
            (".WORKTREES/foo/src/lib.rs", false, false),
            (".acdd/state.sqlite", false, false),
            (".ACDD/STATE.SQLITE", false, false),
            (".acdd/state.sqlite-wal", false, false),
            (".ACDD/State.sqlite-WAL", false, false),
            (".acdd/state.sqlite-shm", false, false),
            (".acdd/state.sqlite.backup", false, false),
            ("custom/index/code-map.sqlite", false, false),
            ("CUSTOM/INDEX/CODE-MAP.SQLITE-SHM", false, false),
            ("custom/index/code-map.sqlite-wal", false, false),
            ("custom/index/code-map.sqlite-shm", false, false),
            ("custom/index/code-map.sqlite.backup", false, false),
            (".acdd/run/r1/traces.jsonl", false, false),
            (".acdd/tmp/scratch.json", false, false),
            (".acdd/profile.yaml", false, true),
            (".ACDD/profile.yaml", false, false),
            (".acdd/profiles/custom.yaml", false, true),
            (".acdd/frameworks/django.yaml", false, true),
            ("Cargo.lock", false, true),
            ("tests/acdd/fixtures/events.jsonl", false, true),
            ("/etc/passwd", false, false),
            ("../outside.rs", false, false),
            ("", false, false),
        ];

        for &(path, expect_managed, expect_included) in cases {
            assert_eq!(
                policy.is_engine_managed(path),
                expect_managed,
                "is_engine_managed({path:?})"
            );
            assert_eq!(
                policy.is_candidate_included(path),
                expect_included,
                "is_candidate_included({path:?})"
            );
        }
    }

    #[test]
    fn path_policy_scope_and_agent_writable_matrix() {
        let policy = WorkspacePathPolicy::new(
            ["docs/milestones/AC-008.md".to_owned()],
            ".acdd/state.sqlite",
            ".acdd/code-map.sqlite",
        );
        let protected = vec!["Cargo.toml".to_owned(), "build.rs".to_owned()];

        // (path, admitted_scope, expected_scope_admitted, expected_agent_writable)
        let cases: &[(&str, &[&str], bool, bool)] = &[
            ("crates/acdd-core/src/lib.rs", &["crates/acdd-core/**"], true, true),
            ("crates/acdd-core/src/lib.rs", &["crates/acdd-core"], true, true),
            ("crates/acdd-core-extra/src/lib.rs", &["crates/acdd-core"], false, false),
            ("crates/acdd-core/Cargo.toml", &["crates/acdd-core/**"], true, true),
            ("Cargo.toml", &["Cargo.toml"], true, false),
            ("CARGO.TOML", &["**"], true, false),
            ("build.rs", &["**"], true, false),
            ("BUILD.RS", &["**"], true, false),
            ("docs/milestones/AC-008.md", &["docs/milestones/**"], true, false),
            ("DOCS/MILESTONES/AC-008.MD", &["**"], true, false),
            (".acdd/state.sqlite", &[".acdd/**", "**"], true, false),
            (".ACDD/state.sqlite", &["**"], true, false),
            (".acdd/run/r1/traces.jsonl", &[".acdd/**"], true, false),
            (".acdd/profile.yaml", &[".acdd/profile.yaml"], true, true),
            (".acdd/profile.yaml", &["src/**"], false, false),
            (".acdd/profiles/team.yaml", &[".acdd/profiles/**"], true, true),
            ("tests/a/b/fixture.jsonl", &["tests/**/*.jsonl"], true, true),
            ("tests/fixture.jsonl", &["tests/**/*.jsonl"], true, true),
            ("AGENTS.md", &["**/AGENTS.md"], true, true),
            ("docs/AGENTS.md", &["**/AGENTS.md"], true, true),
            (".git/config", &["**"], true, false),
            (".GIT/config", &["**"], true, false),
            (".worktrees/w1/src/lib.rs", &["**"], true, false),
            ("../escape.rs", &["**"], false, false),
        ];

        for &(path, scope_slice, expect_admitted, expect_writable) in cases {
            let scope: Vec<String> = scope_slice.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(
                policy.is_scope_admitted(path, &scope),
                expect_admitted,
                "is_scope_admitted({path:?}, {scope:?})"
            );
            assert_eq!(
                policy.is_agent_writable(path, &scope, &protected),
                expect_writable,
                "is_agent_writable({path:?}, {scope:?})"
            );
        }

        // Canonical APFS-safe helpers & `is_valid_report_path` matrix (strictly `.acdd/reports/*.{md,json}`).
        for &(path, expect_json, expect_valid_report) in &[
            (".acdd/reports/run-1.md", false, true),
            (".acdd/reports/run-1.JSON", true, true),
            (".acdd/reports/nested/run-1.MD", false, true),
            (".ACDD/reports/run-1.md", false, false),
            (".acdd/reports/run-1.txt", false, false),
            (".acdd/reports", false, false),
            ("reports/AC-001.json", true, false),
            ("reports/AC-001.JSON", true, false),
            (".acdd/state.sqlite", false, false),
            (".ACDD/state.sqlite", false, false),
            (".acdd/code-map.sqlite", false, false),
            (".acdd/profile.yaml", false, false),
            (".git/config", false, false),
            (".GIT/HEAD", false, false),
            ("Cargo.toml", false, false),
            ("CARGO.TOML", false, false),
            ("docs/milestones/AC-008.md", false, false),
            ("../escape.md", false, false),
        ] {
            assert_eq!(has_json_extension(path), expect_json, "has_json_extension({path:?})");
            assert_eq!(
                policy.is_valid_report_path(path, &protected),
                expect_valid_report,
                "is_valid_report_path({path:?})"
            );
        }
    }

    #[test]
    fn glob_brace_expansion_matching_and_overlap_matrix() {
        // 1. Brace expansion cases (including nested, sequential, unbalanced, empty, and >64 cap).
        let huge = "{a,b}/{c,d}/{e,f}/{g,h}/{i,j}/{k,l}/{m,n}.rs"; // 128 > 64 cap
        let expand_cases: &[(&str, &[&str])] = &[
            ("src/{a,b}.rs", &["src/a.rs", "src/b.rs"]),
            (
                "crates/{a,{b,c}}/*.rs",
                &["crates/a/*.rs", "crates/b/*.rs", "crates/c/*.rs"],
            ),
            (
                "src/{a,b}/{c,d}.rs",
                &["src/a/c.rs", "src/a/d.rs", "src/b/c.rs", "src/b/d.rs"],
            ),
            ("src/{,sub/}lib.rs", &["src/lib.rs", "src/sub/lib.rs"]),
            ("src/{}.rs", &[]),
            ("src/{a,b.rs", &[]),
            ("src/a,b}.rs", &[]),
            (huge, &[]),
        ];
        for &(raw, expected) in expand_cases {
            let actual: Vec<String> = expand_glob_braces(raw)
                .into_iter()
                .map(|s| normalize_rel_path(&s).unwrap_or(s))
                .collect();
            let exp: Vec<String> = expected.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(actual, exp, "expand_glob_braces({raw:?})");
        }

        // 1b. Canonical scope glob validation & fail-closed protected_paths.
        let valid_cases: &[(&str, bool)] = &[
            ("src/**", true),
            ("src/{a,b}.rs", true),
            ("src/{,sub/}lib.rs", true),
            ("", false),
            ("   ", false),
            (".", false),
            ("./", false),
            ("../escape/**", false),
            ("/etc/passwd", false),
            ("C:/repo/src", false),
            ("src/foo\0bar.rs", false),
            ("src/{a", false),
            ("src/a}", false),
            ("src/{}.rs", false),
            ("src/{a,../b}.rs", false),
            (huge, false),
        ];
        for &(pat, expected) in valid_cases {
            assert_eq!(
                is_valid_scope_glob(pat),
                expected,
                "is_valid_scope_glob({pat:?})"
            );
        }
        let policy = WorkspacePathPolicy::default();
        assert!(!policy.is_agent_writable(
            "src/lib.rs",
            &["src/**".to_owned()],
            &["src/{a".to_owned()],
        ));
        assert!(!policy.is_agent_writable(
            "src/lib.rs",
            &["src/**".to_owned()],
            &[huge.to_owned()],
        ));

        // 2. glob_matches & is_scope_admitted with braces.
        let match_cases: &[(&str, &str, bool)] = &[
            ("crates/{acdd-core,acdd-state}/**", "crates/acdd-core/src/lib.rs", true),
            ("crates/{acdd-core,acdd-state}/**", "crates/acdd-state/src/lib.rs", true),
            ("crates/{acdd-core,acdd-state}/**", "crates/acdd-index/src/lib.rs", false),
            ("src/{a,{b,c}}/*.{rs,md}", "src/c/mod.md", true),
            ("src/{a,{b,c}}/*.{rs,md}", "src/c/mod.py", false),
            ("src/{a,b", "src/a", false),
        ];
        for &(pat, path, expected) in match_cases {
            assert_eq!(glob_matches(pat, path), expected, "glob_matches({pat:?}, {path:?})");
            assert_eq!(
                policy.is_scope_admitted(path, &[pat.to_owned()]),
                expected,
                "is_scope_admitted({path:?}, {pat:?})"
            );
        }

        // 3. patterns_overlap & scopes_overlap with braces and segment wildcards.
        let overlap_cases: &[(&str, &str, bool)] = &[
            ("crates/{a,b}/**", "crates/b/src/lib.rs", true),
            ("crates/{a,b}/**", "crates/c/**", false),
            ("crates/a/*.{rs,toml}", "crates/a/foo.rs", true),
            ("crates/a/*.{rs,toml}", "crates/a/*.md", false),
            ("crates/a", "crates/{a,b}/src/lib.rs", true),
            ("crates/a", "crates/ab/**", false),
            ("**", "Cargo.toml", true),
        ];
        for &(a, b, expected) in overlap_cases {
            assert_eq!(patterns_overlap(a, b), expected, "patterns_overlap({a:?}, {b:?})");
            assert_eq!(
                scopes_overlap(&[a.to_owned()], &[b.to_owned()]),
                expected,
                "scopes_overlap({a:?}, {b:?})"
            );
        }
    }

    #[test]
    fn from_workspace_config_normalizes_and_defaults_blank_db_paths() {
        let cases: &[(&str, &str, &[&str], &str, &str)] = &[
            (
                "./custom/state.sqlite",
                "custom/code-map.sqlite",
                &["docs/milestones/AC-001.md", "docs/milestones/archive/AC-001.md"],
                "custom/state.sqlite",
                "custom/code-map.sqlite",
            ),
            (
                "   ",
                "",
                &["docs/plans/PLAN-001.md"],
                ".acdd/state.sqlite",
                ".acdd/code-map.sqlite",
            ),
        ];
        for &(state_rel, code_rel, managed, expect_state, expect_code) in cases {
            let policy = WorkspacePathPolicy::from_workspace_config(
                state_rel,
                code_rel,
                managed.iter().map(|s| (*s).to_owned()),
            );
            assert_eq!(policy.state_db_path, expect_state);
            assert_eq!(policy.code_db_path, expect_code);
            for path in managed {
                assert!(policy.is_engine_managed(path), "{path}");
                assert!(!policy.is_candidate_included(path), "{path}");
            }
            assert!(!policy.is_candidate_included(expect_state));
            assert!(!policy.is_candidate_included(&format!("{expect_state}-wal")));
            assert!(!policy.is_candidate_included(expect_code));
        }
    }

    #[test]
    fn workspace_path_policy_json_backwards_compatibility_and_limits_matrix() {
        let legacy_json = r#"{
            "engine_managed_paths": ["docs/milestones/AC-001.md"],
            "state_db_path": ".acdd/state.sqlite",
            "code_db_path": ".acdd/code-map.sqlite"
        }"#;
        let legacy: WorkspacePathPolicy =
            serde_json::from_str(legacy_json).expect("deserialize legacy JSON");
        assert_eq!(legacy.max_snapshot_file_bytes, None);
        assert_eq!(legacy.allowed_binary_extensions, None);
        assert!(legacy.is_binary_extension_allowed("src/evil.bin"));

        let default_json =
            serde_json::to_value(WorkspacePathPolicy::default()).expect("serialize default");
        assert!(default_json.get("max_snapshot_file_bytes").is_none());
        assert!(default_json.get("allowed_binary_extensions").is_none());

        let configured = WorkspacePathPolicy::default().with_snapshot_limits(
            Some(4096),
            Some(BTreeSet::from([".PNG".to_owned(), "mo".to_owned()])),
        );
        let roundtrip: WorkspacePathPolicy =
            serde_json::from_value(serde_json::to_value(&configured).expect("serialize"))
                .expect("roundtrip");
        assert_eq!(roundtrip.max_snapshot_file_bytes, Some(4096));
        assert_eq!(
            roundtrip.allowed_binary_extensions,
            Some(BTreeSet::from(["mo".to_owned(), "png".to_owned()]))
        );
        assert!(roundtrip.is_binary_extension_allowed("assets/logo.PNG"));
        assert!(roundtrip.is_binary_extension_allowed("locale/django.mo"));
        assert!(!roundtrip.is_binary_extension_allowed("src/evil.bin"));
        assert!(!roundtrip.is_binary_extension_allowed("no_extension"));
    }
}
