//! Specification drafting and slug sanitization (`draft_spec`).

use crate::discovery::{effective_spec_collections, next_spec_id_for_prefix};
use crate::overlay::SpecRuntimeOverlay;
use crate::spec::parse_spec;
use crate::types::{DraftReq, DraftResp};
use acdd_core::config::SpecConfig;
use acdd_core::errors::{ErrorCode, AcddError};
use std::collections::BTreeMap;
use std::path::Path;

/// Normalize a user-supplied draft slug into a bounded lowercase `[a-z0-9_-]` identifier.
fn sanitize_slug(raw: &str) -> Result<String, AcddError> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let slug: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() || slug.len() > 64 {
        return Err(AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("invalid draft slug '{raw}'"),
        ));
    }
    Ok(slug)
}

/// Draft a new sequentially numbered specification Markdown document inside `primary`.
pub fn draft_spec(
    primary: &Path,
    configured_specs: &BTreeMap<String, SpecConfig>,
    req: &DraftReq,
    overlay: &dyn SpecRuntimeOverlay,
) -> Result<DraftResp, AcddError> {
    let collections = effective_spec_collections(configured_specs);
    let col = collections.get(&req.spec).ok_or_else(|| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!("unknown spec collection '{}'", req.spec),
        )
    })?;

    let slug = sanitize_slug(&req.slug)?;
    let draft_dir = col.default_draft_dir().ok_or_else(|| {
        AcddError::new(
            ErrorCode::SpecSchemaInvalid,
            format!(
                "spec collection '{}' has no literal non-glob directory for drafting",
                req.spec
            ),
        )
    })?;

    let (spec_id, spec_rel) = if col.prefixes.is_empty() && req.prefix.is_none() {
        (slug.clone(), format!("{draft_dir}/{slug}.md"))
    } else {
        let prefix = match req.prefix {
            Some(ref p) => {
                if !col.prefixes.is_empty() && !col.prefixes.contains(p) {
                    return Err(AcddError::new(
                        ErrorCode::SpecSchemaInvalid,
                        format!(
                            "prefix '{p}' is not allowed for collection '{}' (allowed: {:?})",
                            req.spec, col.prefixes
                        ),
                    ));
                }
                p.clone()
            }
            None => col
                .prefixes
                .first()
                .cloned()
                .unwrap_or_else(|| "SPEC".to_owned()),
        };

        let db_max = overlay.max_spec_number(&req.spec, &prefix)?;
        let spec_id = next_spec_id_for_prefix(primary, &req.spec, col, &prefix, db_max)?;
        let spec_rel = format!("{draft_dir}/{spec_id}-{slug}.md");
        (spec_id, spec_rel)
    };

    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| slug.replace('-', " "));
    let workflow_line = req
        .workflow
        .as_deref()
        .or(col.workflow.as_deref())
        .map(|w| format!("workflow: {w}\n"))
        .unwrap_or_default();

    let rendered = format!(
        "---\nid: {spec_id}\ntitle: \"{}\"\ndoc_type: contract\n{workflow_line}status: planned\ninvariants: []\nacceptance_criteria: []\n---\n\n# {spec_id}: {title}\n\n### task: T1\n\n```yaml\ntarget: \"Describe task T1 target\"\nscope: []\ntags: [default]\ninvariants: []\nacceptance_criteria: []\n```\n",
        title.replace('"', "\\\"")
    );
    let _ = parse_spec(&rendered)?;

    match acdd_core::fs::write_new_confined(primary, Path::new(&spec_rel), rendered.as_bytes()) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("draft target '{spec_rel}' already exists"),
            ));
        }
        Err(err) => {
            return Err(AcddError::new(
                ErrorCode::SpecSchemaInvalid,
                format!("failed to write drafted spec '{spec_rel}': {err}"),
            ));
        }
    }

    Ok(DraftResp {
        spec_id,
        spec_path: spec_rel,
        spec: req.spec.clone(),
    })
}
