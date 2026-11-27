//! Canonical YAML frontmatter extraction shared by `acdd-index` and `acdd-spec`.
//!
//! `ARCH-INDEX-SPEC-ENTRY` confines `crates/acdd-index` imports of `acdd-spec` strictly to this module.

use acdd_core::errors::{ErrorCode, AcddError};

/// Reason YAML frontmatter could not be split from a Markdown document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmatterMissing {
    /// Document did not begin with an opening `---` line.
    MissingOpeningFence,
    /// Document opened `---` without a matching closing `---` line.
    MissingClosingFence,
}

/// Zero-copy slice result returned by [`split_frontmatter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontmatterSplit<'a> {
    /// Raw YAML slice between the opening and closing `---` lines, if present.
    pub yaml: Option<&'a str>,
    /// Remaining Markdown body slice (entire input when frontmatter is absent or unclosed).
    pub body: &'a str,
    /// Byte offset of `body` within the original `source`.
    pub body_offset: usize,
    /// Diagnostic reason when `yaml` is `None`.
    pub missing_reason: Option<FrontmatterMissing>,
}

/// Split optional leading YAML frontmatter (`---` ... `---`) from `source` without failing.
///
/// Strips an optional leading UTF-8 BOM while keeping `body_offset` relative to `source`.
pub fn split_frontmatter(source: &str) -> FrontmatterSplit<'_> {
    let normalized = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(rest) = normalized
        .strip_prefix("---\n")
        .or_else(|| normalized.strip_prefix("---\r\n"))
    else {
        return FrontmatterSplit {
            yaml: None,
            body: source,
            body_offset: 0,
            missing_reason: Some(FrontmatterMissing::MissingOpeningFence),
        };
    };

    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let body_offset = source.len() - body.len();
            return FrontmatterSplit {
                yaml: Some(yaml),
                body,
                body_offset,
                missing_reason: None,
            };
        }
        offset += line.len();
    }

    FrontmatterSplit {
        yaml: None,
        body: source,
        body_offset: 0,
        missing_reason: Some(FrontmatterMissing::MissingClosingFence),
    }
}

/// Split mandatory YAML frontmatter (`---` ... `---`) from a specification document, failing closed when absent or unclosed.
pub fn split_frontmatter_required(raw: &str) -> Result<(&str, &str), AcddError> {
    let split = split_frontmatter(raw);
    match (split.yaml, split.missing_reason) {
        (Some(fm), _) => Ok((fm, split.body)),
        (None, Some(FrontmatterMissing::MissingClosingFence)) => Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            "spec document frontmatter is missing closing '---'",
        )),
        (None, _) => Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            "spec document must start with YAML frontmatter '---'",
        )),
    }
}
