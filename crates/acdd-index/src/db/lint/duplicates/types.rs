use crate::models::StatementFingerprint;
use std::collections::BTreeSet;

pub(crate) struct FileDuplicateGroup {
    pub(crate) digest: String,
    pub(crate) size_bytes: u64,
    pub(crate) locations: Vec<String>,
}

pub(crate) struct DuplicateCandidate {
    pub(crate) name: String,
    pub(crate) qualname: String,
    pub(crate) kind: String,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
    pub(crate) path: String,
    pub(crate) signature: String,
    pub(crate) inbound_callers: usize,
    pub(crate) impl_trait: Option<String>,
    pub(crate) is_standard_mapper_impl: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct DuplicateLocation {
    pub(crate) candidate_idx: usize,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
}

pub(crate) struct DuplicateGroup {
    pub(crate) rep_idx: usize,
    pub(crate) line_count: usize,
    pub(crate) match_type: &'static str,
    pub(crate) locations: Vec<DuplicateLocation>,
    pub(crate) duplicated_lines: usize,
}

/// Graph-ranked partial-similarity (`0.45..0.80`) reuse hint pointing to an existing canonical owner.
#[derive(Debug, Clone)]
pub(crate) struct ReuseHint {
    pub(crate) probe_name: String,
    pub(crate) probe_path: String,
    pub(crate) probe_line: usize,
    pub(crate) owner_name: String,
    pub(crate) owner_path: String,
    pub(crate) owner_line: usize,
    pub(crate) owner_signature: String,
    pub(crate) inbound_callers: usize,
    pub(crate) similarity: f64,
}

/// Explicit candidate diff provenance for diff-aware lint checks.
#[derive(Debug, Clone, Copy)]
pub struct LintDiffContext<'a> {
    pub diff_paths: &'a [String],
    pub baseline_ref: Option<&'a str>,
}

/// Unified request for asymmetric Probe-vs-Workspace duplicate scanning.
pub(crate) struct DuplicateScanRequest<'a> {
    pub(crate) scope: &'a str,
    pub(crate) probe_paths: Option<&'a [String]>,
    pub(crate) min_lines: usize,
}

pub(crate) struct SymbolDuplicateReport {
    pub(crate) candidates: Vec<DuplicateCandidate>,
    pub(crate) candidate_statements: Vec<Vec<StatementFingerprint>>,
    pub(crate) groups: Vec<DuplicateGroup>,
    pub(crate) reuse_hints: Vec<ReuseHint>,
    pub(crate) total_duplicated_lines: usize,
    pub(crate) stale_files: BTreeSet<String>,
}

pub(super) fn effective_stmt_hash(s: &StatementFingerprint, use_skeleton: bool) -> u64 {
    if use_skeleton && s.skeleton_hash != 0 {
        s.skeleton_hash
    } else {
        s.hash
    }
}

pub(super) fn path_matches_probe(path: &str, scope: &str, probe_paths: Option<&[String]>) -> bool {
    if let Some(paths) = probe_paths {
        if !paths.is_empty() {
            return paths.iter().any(|p| {
                let p = p.trim_end_matches('/');
                path == p || path.starts_with(&format!("{p}/"))
            });
        }
    }
    if scope.is_empty() {
        return true;
    }
    let s = scope.trim_end_matches('/');
    path == s || path.starts_with(&format!("{s}/"))
}
