//! Shared support for integration tests.

pub mod assertions;
pub mod cli;
pub mod mcp_client;
pub mod mcp_harness;
pub mod sqlite;
pub mod workspace;

pub use workspace::Workspace;

/// Removes ambient `ACDD_*` and `GIT_*` environment variables not explicitly configured on `command`.
#[allow(dead_code)]
pub fn isolate_acdd_env(command: &mut std::process::Command) -> &mut std::process::Command {
    let explicit: std::collections::HashSet<_> = command
        .get_envs()
        .map(|(key, _)| key.to_os_string())
        .collect();
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if (name.starts_with("ACDD_") || name.starts_with("GIT_")) && !explicit.contains(&key) {
            command.env_remove(key);
        }
    }
    command
}

/// Test helper wrapping [`acdd_index::linker::link_with_registry`] without shipping a test-only
/// convenience wrapper in `acdd_index`.
#[allow(dead_code)]
pub fn link_facts(
    all: &std::collections::BTreeMap<String, acdd_index::models::Facts>,
) -> acdd_index::models::Graph {
    link_facts_with_root(all, None, None)
}

/// Test helper wrapping [`acdd_index::linker::link_with_registry`] with optional owners/root.
#[allow(dead_code)]
pub fn link_facts_with_root(
    all: &std::collections::BTreeMap<String, acdd_index::models::Facts>,
    owners: Option<&std::collections::BTreeSet<String>>,
    root: Option<&std::path::Path>,
) -> acdd_index::models::Graph {
    let dependencies =
        acdd_index::languages::dependency_registry::DependencyRegistry::collect(root);
    acdd_index::linker::link_with_registry(all, owners, root, &dependencies)
}

/// Test helper wrapping [`acdd_index::linker::link_compact_catalog_with_registry`].
#[allow(dead_code)]
pub fn link_catalog(catalog: &acdd_index::typed_facts::SymbolCatalog) -> acdd_index::models::Graph {
    let dependencies =
        acdd_index::languages::dependency_registry::DependencyRegistry::collect(None);
    acdd_index::linker::link_compact_catalog_with_registry(catalog, None, &dependencies).into()
}

/// Test helper decoding persisted `local_facts` payloads via [`acdd_index::typed_facts::decode_facts`].
#[allow(dead_code)]
pub fn decode_local_facts(blob: &[u8]) -> anyhow::Result<acdd_index::models::Facts> {
    Ok(acdd_index::typed_facts::decode_facts(blob)?.into_public())
}

/// Test helper resolving one selector via [`acdd_index::db::reader::inspect_paged_with_path`].
#[allow(dead_code)]
pub fn select_node(
    conn: &rusqlite::Connection,
    selector: &str,
) -> anyhow::Result<serde_json::Value> {
    let options = acdd_core::response::QueryOptions::resolve(
        &acdd_core::response::ResponsePolicy::default(),
        Some(1),
        0,
        Some(acdd_core::response::Detail::Full),
        None,
    )?;
    Ok(
        acdd_index::db::reader::inspect_paged_with_path(conn, selector, None, false, &options)?
            ["node"]
            .clone(),
    )
}
