//! Shared leaf pipeline types, snapshot verification, component ownership, and DB target guards.

use acdd_core::config::{resolve_file_path, Adapter};
use acdd_index::{
    ast, commitments, docs,
    languages::dependency_registry::DependencyRegistry,
    models::{Edge, Facts, FileEntry, Node},
    typed_facts::TypedFacts,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicU64,
};

pub(super) static NONCE: AtomicU64 = AtomicU64::new(0);
pub(super) const EXTRACTION_MEMORY_MULTIPLIER: u64 = 8;

/// The indexing operation being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum BuildMode {
    /// Build an index from a complete scan.
    Cold,
    /// Update an existing index from changed paths.
    IncrementalDelta,
}

impl BuildMode {
    /// Returns static string identifier of the build mode.
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            BuildMode::Cold => "Cold",
            BuildMode::IncrementalDelta => "IncrementalDelta",
        }
    }
}

impl std::fmt::Display for BuildMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug)]
/// Reports that a source file changed after its inventory digest was scanned.
pub struct SourceSnapshotMismatch(pub String);

impl std::fmt::Display for SourceSnapshotMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SourceSnapshotMismatch {}

/// Removes classic value flow on globals before catalog storage or linking.
pub(super) fn sanitize_extracted_facts(facts: &mut TypedFacts) {
    for node in &mut facts.facts.nodes {
        if node.details.get("classic_global") == Some(&Value::Bool(true)) {
            if let Some(details) = node.details.as_object_mut() {
                details.remove("value_flow");
            }
        }
    }
}

/// Reads a workspace file with `O_NOFOLLOW` into `source`, verifying `expected_bytes` (when given)
/// and `max_file_bytes` without copying.
pub(super) fn open_and_read_verified_source(
    root: &Path,
    adapter: &Adapter,
    rel_path: &str,
    reserve_bytes: u64,
    expected_bytes: Option<u64>,
    source: &mut String,
) -> Result<fs::File> {
    let path = resolve_file_path(root, adapter, rel_path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut source_file = options.open(&path)?;
    let source_capacity =
        usize::try_from(reserve_bytes).context("source size exceeds addressable memory")?;
    source.clear();
    source.try_reserve_exact(source_capacity)?;
    source_file
        .by_ref()
        .take(adapter.limits.max_file_bytes.saturating_add(1))
        .read_to_string(source)?;
    if source.len() as u64 > adapter.limits.max_file_bytes
        || expected_bytes.is_some_and(|b| source.len() as u64 != b)
    {
        return Err(
            SourceSnapshotMismatch(format!("source changed during extraction: {rel_path}")).into(),
        );
    }
    Ok(source_file)
}

/// Extracts typed facts from an in-memory source slice whose digest already matches `file.digest`.
pub(super) fn extract_typed_source_with_digest(
    file: &FileEntry,
    source: &str,
    source_digest: &str,
    dependencies: &DependencyRegistry,
    spec_paths: &[String],
    known_zones: Option<&BTreeSet<String>>,
) -> Result<TypedFacts> {
    if source_digest != file.digest {
        return Err(SourceSnapshotMismatch(format!(
            "source changed during extraction: {}",
            file.path
        ))
        .into());
    }
    let mut typed = if file.is_doc {
        let parsed = docs::extract_with_details_with_zones(
            &file.path,
            source,
            file.mtime_ns as f64 / 1e9,
            spec_paths,
            known_zones,
        )?;
        let sections = parsed
            .iter()
            .map(|(doc, _)| doc.clone())
            .collect::<Vec<_>>();
        let mut nodes = Vec::new();
        let mut errors = Vec::new();
        if let Some(err_msg) = parsed
            .first()
            .and_then(|(_, metadata)| metadata.get("frontmatter_error"))
            .and_then(Value::as_str)
        {
            errors.push(acdd_index::models::Diagnostic {
                path: file.path.clone(),
                line: 1,
                message: format!("invalid frontmatter: {err_msg}"),
            });
        }
        for (doc, metadata) in &parsed {
            let line = metadata["line"].as_u64().unwrap_or(1) as usize;
            let end_line = metadata["end_line"].as_u64().unwrap_or(line as u64) as usize;
            if let Some(rule_errors) = metadata
                .get("arch_rule_errors")
                .and_then(Value::as_array)
            {
                for err in rule_errors {
                    let err_line = err["line"].as_u64().unwrap_or(line as u64) as usize;
                    if let Some(msg) = err["message"].as_str() {
                        errors.push(acdd_index::models::Diagnostic {
                            path: file.path.clone(),
                            line: err_line,
                            message: msg.to_owned(),
                        });
                    }
                }
            }
            let mut details = json!({
                "doc_type": doc.doc_type,
                "is_invariant": doc.is_invariant,
                "status": metadata["status"],
                "citations": metadata["citations"],
                "symbol_claims": metadata["symbol_claims"],
                "declared_invariants": metadata["declared_invariants"],
            });
            if metadata["status"].is_null() {
                details.as_object_mut().unwrap().remove("status");
            }
            if let Some(rules) = metadata
                .get(acdd_index::models::details::NodeDetailKey::ARCH_RULES)
                .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
            {
                details.as_object_mut().unwrap().insert(
                    acdd_index::models::details::NodeDetailKey::ARCH_RULES.to_owned(),
                    rules.clone(),
                );
            }
            nodes.push(Node {
                native_semantic: None,
                id: doc.doc_id.clone(),
                kind: "document".into(),
                name: doc.section_title.clone(),
                qualname: doc.doc_id.clone(),
                path: doc.path.clone(),
                line,
                end_line,
                is_test: false,
                language: "markdown".into(),
                generated: false,
                details,
            });
            if let Some(invariants) = metadata["declared_invariants"].as_array() {
                for invariant in invariants {
                    let Some(name) = invariant["id"].as_str() else {
                        continue;
                    };
                    let invariant_line = invariant["line"].as_u64().unwrap_or(line as u64) as usize;
                    nodes.push(Node {
                        native_semantic: None,
                        id: format!("invariant:{}:{invariant_line}:{name}", doc.path),
                        kind: "invariant".into(),
                        name: name.to_owned(),
                        qualname: format!("{}::{name}", doc.doc_id),
                        path: doc.path.clone(),
                        line: invariant_line,
                        end_line: invariant_line,
                        is_test: false,
                        language: "markdown".into(),
                        generated: false,
                        details: json!({
                            "document_id": doc.doc_id,
                            "rule": invariant["rule"],
                            "status": metadata["status"],
                        }),
                    });
                }
            }
        }
        TypedFacts::from_public(Facts {
            docs: sections,
            nodes,
            errors,
            ..Facts::default()
        })
    } else {
        ast::extract_typed_with_registry(&file.path, &file.language, source, dependencies)?
    };
    sanitize_extracted_facts(&mut typed);
    Ok(typed)
}

/// Assigns the full-build component ownership graph to extracted typed facts.
///
/// Replaces existing component nodes and edges using the same deterministic
/// owner selection as a filesystem full build. Returns component owner paths.
pub fn components(facts: &mut BTreeMap<String, TypedFacts>) -> BTreeSet<String> {
    let owners = component_owners(facts.iter().filter_map(|(path, f)| {
        f.nodes
            .iter()
            .any(|n| n.kind == "module")
            .then_some(path.as_str())
    }));
    assign_components(facts, &owners);
    owners.into_values().collect()
}

pub(super) fn component_owners<'a>(
    paths: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for path in paths {
        let root = path.split_once('/').map_or(".", |(root, _)| root);
        owners
            .entry(root.to_owned())
            .and_modify(|owner| {
                if path < owner.as_str() {
                    *owner = path.to_owned();
                }
            })
            .or_insert_with(|| path.to_owned());
    }
    owners
}

pub(super) fn assign_file_component(
    path: &str,
    f: &mut TypedFacts,
    owners: Option<&BTreeMap<String, String>>,
) {
    f.facts.retain_nodes(|n| n.kind != "component");
    f.edges.retain(|e| !e.src.starts_with("component:"));
    if !f.nodes.iter().any(|n| n.kind == "module") {
        return;
    }
    let root = path.split_once('/').map_or(".", |(root, _)| root);
    let id = format!("component:{root}");
    if owners.is_some_and(|owners| owners.get(root).map(String::as_str) == Some(path)) {
        f.nodes.push(Node {
            native_semantic: None,
            id: id.clone(),
            kind: "component".into(),
            name: root.into(),
            qualname: root.into(),
            path: root.into(),
            line: 1,
            end_line: 1,
            is_test: false,
            language: "workspace".into(),
            generated: false,
            details: Value::Null,
        });
    }
    f.edges.push(Edge {
        src: id,
        dst: format!("module:{path}"),
        kind: "contains".into(),
        path: path.to_owned(),
        line: 1,
        evidence: "component ownership".into(),
        confidence: "exact".into(),
    });
}

pub(super) fn assign_components(
    facts: &mut BTreeMap<String, TypedFacts>,
    owners: &BTreeMap<String, String>,
) {
    for (path, f) in facts {
        assign_file_component(path, f, Some(owners));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SidecarDisposition {
    Reject,
    Allow,
    Remove,
}

pub(super) fn manage_publication_sidecars(
    output: &Path,
    disposition: SidecarDisposition,
) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = output.as_os_str().to_owned();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match fs::symlink_metadata(&sidecar) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("inspect sidecar {}", sidecar.display()))
            }
            Ok(metadata) if metadata.is_file() && disposition == SidecarDisposition::Allow => {}
            Ok(metadata) if metadata.is_file() && disposition == SidecarDisposition::Remove => {
                fs::remove_file(&sidecar)
                    .with_context(|| format!("remove sidecar {}", sidecar.display()))?;
            }
            Ok(_) => bail!(
                "cannot replace database with existing sidecar {}",
                sidecar.display()
            ),
        }
    }
    Ok(())
}

pub(super) fn hash_source_bytes(source: &str) -> String {
    commitments::hash(source.as_bytes())
}
