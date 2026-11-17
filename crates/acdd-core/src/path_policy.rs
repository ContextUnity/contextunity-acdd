use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Normalized pure workspace path policy implementing the four non-conflated predicates from `ADR-0002`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePathPolicy {
    /// Active `Run` `spec_path` and `archive_path` entries managed exclusively by the state engine.
    #[serde(default)]
    pub engine_managed_paths: BTreeSet<String>,
    /// Configured normalized relative path to the state SQLite database.
    pub state_db_path: String,
    /// Configured normalized relative path to the code-map SQLite database.
    pub code_db_path: String,
}

impl Default for WorkspacePathPolicy {
    fn default() -> Self {
        Self {
            engine_managed_paths: BTreeSet::new(),
            state_db_path: ".acdd/state.sqlite".to_owned(),
            code_db_path: ".acdd/code-map.sqlite".to_owned(),
        }
    }
}

impl WorkspacePathPolicy {
    /// Construct a `WorkspacePathPolicy`, normalizing all supplied relative paths.
    pub fn new(
        engine_managed_paths: impl IntoIterator<Item = String>,
        state_db_path: impl AsRef<str>,
        code_db_path: impl AsRef<str>,
    ) -> Self {
        let managed = engine_managed_paths
            .into_iter()
            .filter_map(|p| normalize_rel_path(&p))
            .collect();
        let state_db = normalize_rel_path(state_db_path.as_ref())
            .unwrap_or_else(|| ".acdd/state.sqlite".to_owned());
        let code_db = normalize_rel_path(code_db_path.as_ref())
            .unwrap_or_else(|| ".acdd/code-map.sqlite".to_owned());
        Self {
            engine_managed_paths: managed,
            state_db_path: state_db,
            code_db_path: code_db,
        }
    }

    /// Predicate 1: Return `true` if `rel_path` matches any active `Run`'s `spec_path` or `archive_path`.
    ///
    /// Human edits to engine-managed specs during an active `Run` are reconciled strictly via `acdd sync`.
    pub fn is_engine_managed(&self, rel_path: &str) -> bool {
        normalize_rel_path(rel_path)
            .is_some_and(|norm| self.engine_managed_paths.contains(&norm))
    }

    /// Predicate 2: Return `true` if `rel_path` belongs in candidate/snapshot `TreeOid` calculations.
    ///
    /// Excludes `.git/**`, `.worktrees/**`, `is_engine_managed(rel_path)`, configured `state_db`/`code_db`
    /// (plus `-wal`, `-shm`, `.backup` sidecars), and `.acdd/**` EXCEPT the tracked whitelist
    /// `.acdd/profile.yaml`, `.acdd/profiles/**`, and `.acdd/frameworks/**`.
    /// Explicitly includes tracked artifacts such as `Cargo.lock` and `tests/**/*.jsonl`.
    pub fn is_candidate_included(&self, rel_path: &str) -> bool {
        let Some(norm) = normalize_rel_path(rel_path) else {
            return false;
        };
        if is_under_dir(&norm, ".git") || is_under_dir(&norm, ".worktrees") {
            return false;
        }
        if self.engine_managed_paths.contains(&norm) {
            return false;
        }
        if is_db_or_sidecar(&norm, &self.state_db_path)
            || is_db_or_sidecar(&norm, &self.code_db_path)
        {
            return false;
        }
        if is_under_dir(&norm, ".acdd") && !is_acdd_tracked_whitelist(&norm) {
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

    /// Predicate 4: Return `true` if an agent may write `rel_path` under `admitted_scope` and `protected_paths`.
    ///
    /// Forbids `.git/**`, `.worktrees/**`, `is_engine_managed`, `state_db`/`code_db` (+ sidecars),
    /// `protected_paths`, and non-whitelisted `.acdd/**`; requires `is_scope_admitted(rel_path, admitted_scope)`
    /// (so `.acdd/{profile.yaml,profiles/**,frameworks/**}` are writable only when explicitly covered by `admitted_scope`).
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
        if self.is_scope_admitted(&norm, protected_paths) {
            return false;
        }
        self.is_scope_admitted(&norm, admitted_scope)
    }
}

/// Normalize a workspace-relative path purely lexically without touching the filesystem.
///
/// Rejects empty paths, NUL bytes, absolute paths, Windows drive prefixes, and `..` parent traversal.
pub fn normalize_rel_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return None;
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }

    let replaced = trimmed.replace('\\', "/");
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

fn is_under_dir(norm_path: &str, dir: &str) -> bool {
    norm_path == dir
        || (norm_path.len() > dir.len()
            && norm_path.starts_with(dir)
            && norm_path.as_bytes()[dir.len()] == b'/')
}

fn is_db_or_sidecar(norm_path: &str, db_path: &str) -> bool {
    if norm_path == db_path {
        return true;
    }
    let Some(suffix) = norm_path.strip_prefix(db_path) else {
        return false;
    };
    matches!(suffix, "-wal" | "-shm" | ".backup")
}

fn is_acdd_tracked_whitelist(norm_path: &str) -> bool {
    norm_path == ".acdd/profile.yaml"
        || norm_path.starts_with(".acdd/profiles/")
        || norm_path.starts_with(".acdd/frameworks/")
}

fn pattern_matches_segments(raw_pattern: &str, path_segs: &[&str]) -> bool {
    let Some(norm_pat) = normalize_rel_path(raw_pattern) else {
        return false;
    };
    let pat_segs: Vec<&str> = norm_pat.split('/').collect();
    let has_wildcard = pat_segs
        .iter()
        .any(|seg| *seg == "**" || seg.contains('*') || seg.contains('?'));
    if !has_wildcard {
        return path_segs.len() >= pat_segs.len() && path_segs[..pat_segs.len()] == pat_segs[..];
    }
    match_segments(&pat_segs, path_segs)
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
            ("docs/milestones/archive/AC-008.md", true, false),
            ("docs/milestones/AC-009.md", false, true),
            (".git/HEAD", false, false),
            (".worktrees/foo/src/lib.rs", false, false),
            (".acdd/state.sqlite", false, false),
            (".acdd/state.sqlite-wal", false, false),
            (".acdd/state.sqlite-shm", false, false),
            (".acdd/state.sqlite.backup", false, false),
            ("custom/index/code-map.sqlite", false, false),
            ("custom/index/code-map.sqlite-wal", false, false),
            ("custom/index/code-map.sqlite-shm", false, false),
            ("custom/index/code-map.sqlite.backup", false, false),
            (".acdd/run/r1/traces.jsonl", false, false),
            (".acdd/tmp/scratch.json", false, false),
            (".acdd/profile.yaml", false, true),
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
            ("build.rs", &["**"], true, false),
            ("docs/milestones/AC-008.md", &["docs/milestones/**"], true, false),
            (".acdd/state.sqlite", &[".acdd/**", "**"], true, false),
            (".acdd/run/r1/traces.jsonl", &[".acdd/**"], true, false),
            (".acdd/profile.yaml", &[".acdd/profile.yaml"], true, true),
            (".acdd/profile.yaml", &["src/**"], false, false),
            (".acdd/profiles/team.yaml", &[".acdd/profiles/**"], true, true),
            ("tests/a/b/fixture.jsonl", &["tests/**/*.jsonl"], true, true),
            ("tests/fixture.jsonl", &["tests/**/*.jsonl"], true, true),
            ("AGENTS.md", &["**/AGENTS.md"], true, true),
            ("docs/AGENTS.md", &["**/AGENTS.md"], true, true),
            (".git/config", &["**"], true, false),
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
    }
}
