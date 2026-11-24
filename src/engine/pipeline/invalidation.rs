use super::shared::component_owners;
use acdd_core::config;
use acdd_index::{
    languages,
    linker,
    models::{stable_hash64, Facts, FileEntry},
    typed_facts::{decode_facts, TypedFacts},
};
use anyhow::Result;
use rayon::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

struct TempFrontierWriteGuard<'a> {
    conn: &'a Connection,
    restore_query_only: bool,
}

impl<'a> TempFrontierWriteGuard<'a> {
    fn enter(conn: &'a Connection) -> Result<Self> {
        let restore_query_only: bool = conn.query_row("PRAGMA query_only", [], |r| r.get(0))?;
        if restore_query_only {
            conn.execute_batch("PRAGMA query_only = OFF;")?;
        }
        Ok(Self {
            conn,
            restore_query_only,
        })
    }
}

impl Drop for TempFrontierWriteGuard<'_> {
    fn drop(&mut self) {
        if self.restore_query_only {
            let _ = self.conn.execute_batch("PRAGMA query_only = ON;");
        }
    }
}

pub(super) struct TempFrontierTables<'a> {
    conn: &'a Connection,
    tables: Option<Vec<acdd_index::db::writer::temp_table::TempTableGuard<'a>>>,
}

impl Drop for TempFrontierTables<'_> {
    fn drop(&mut self) {
        if let Ok(_write) = TempFrontierWriteGuard::enter(self.conn) {
            drop(self.tables.take());
        }
    }
}

pub(super) fn ensure_temp_frontier_tables(conn: &Connection) -> Result<TempFrontierTables<'_>> {
    use acdd_index::db::writer::temp_table::TempTableGuard;
    let _write = TempFrontierWriteGuard::enter(conn)?;
    let temp_store: i64 = conn.query_row("PRAGMA temp_store", [], |r| r.get(0))?;
    if temp_store != 2 {
        let _ = conn.execute_batch("PRAGMA temp_store = MEMORY;");
    }
    let mut tables = Vec::new();
    for (name, schema) in [
        ("delta_modified_paths", "(key TEXT PRIMARY KEY)"),
        ("delta_changed_resolution", "(key TEXT PRIMARY KEY)"),
        ("delta_frontier_text", "(key TEXT PRIMARY KEY)"),
        ("delta_frontier_text_2", "(key TEXT PRIMARY KEY)"),
        ("delta_frontier_int", "(key INTEGER PRIMARY KEY)"),
    ] {
        tables.push(TempTableGuard::new(conn, name, schema)?);
    }
    Ok(TempFrontierTables { conn, tables: Some(tables) })
}

pub(super) fn populate_temp_frontier<T: rusqlite::ToSql>(
    conn: &Connection,
    table: &str,
    values: impl IntoIterator<Item = T>,
) -> Result<()> {
    let _guard = TempFrontierWriteGuard::enter(conn)?;
    conn.execute_batch(&format!("DELETE FROM temp.{table};"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "INSERT OR IGNORE INTO temp.{table}(key) VALUES(?1)"
    ))?;
    for value in values {
        stmt.execute([value])?;
    }
    Ok(())
}

pub(super) fn populate_temp_text_frontier<'a>(
    conn: &Connection,
    table: &str,
    values: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    populate_temp_frontier(conn, table, values)
}

pub(super) fn populate_temp_int_frontier(
    conn: &Connection,
    table: &str,
    values: impl IntoIterator<Item = i64>,
) -> Result<()> {
    populate_temp_frontier(conn, table, values)
}

pub(super) fn delta_components(
    conn: &Connection,
    modified: &BTreeSet<String>,
    facts: &BTreeMap<String, TypedFacts>,
    replace_files: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, String>> {
    let mut st = conn.prepare("SELECT path.path,owner.path FROM nodes n JOIN path_dictionary path ON path.path_id=n.path_id JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE n.kind='component'")?;
    let previous = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    populate_temp_text_frontier(
        conn,
        "delta_modified_paths",
        modified.iter().map(String::as_str),
    )?;
    let mut st = conn.prepare("SELECT owner.path FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE n.kind='module' AND owner.path NOT IN(SELECT key FROM temp.delta_modified_paths)")?;
    let paths = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let current = component_owners(paths.iter().map(String::as_str).chain(
        facts.iter().filter_map(|(path, f)| {
            f.nodes
                .iter()
                .any(|n| n.kind == acdd_index::models::tags::NodeKind::Module)
                .then_some(path.as_str())
        }),
    ));
    for (root, owner) in &previous {
        if current.get(root) != Some(owner) {
            replace_files.insert(owner.clone());
        }
    }
    for (root, owner) in &current {
        if previous.get(root) != Some(owner) {
            replace_files.insert(owner.clone());
        }
    }
    Ok(current)
}

pub(super) type RustImplShapes = BTreeMap<String, BTreeMap<String, String>>;

pub(super) fn rust_impl_shapes(facts: &Facts) -> Result<RustImplShapes> {
    let imports: BTreeSet<_> = facts
        .references
        .iter()
        .filter(|reference| reference.kind == "imports")
        .map(|reference| {
            (
                reference.expression.as_str(),
                reference.alias.as_deref(),
                reference.module.as_deref(),
            )
        })
        .collect();
    let mut shapes = RustImplShapes::new();
    let mut implementation_keys = std::collections::HashMap::new();
    for implementation in facts.nodes.iter().filter(|node| node.kind == acdd_index::models::tags::NodeKind::Impl) {
        let identity = serde_json::json!({
            "receiver_type": implementation.details["receiver_type"],
            "impl_trait": implementation.details["impl_trait"],
            "generic_impl": implementation.details["generic_impl"],
            "uniform_generic_impl": implementation.details["uniform_generic_impl"],
            "imports": &imports,
        });
        let key = serde_json::to_string(&identity)?;
        implementation_keys.insert(implementation.id.as_str(), key.clone());
        shapes.entry(key).or_default();
    }
    let by_id: std::collections::HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    for edge in facts.edges.iter().filter(|edge| edge.kind == "contains") {
        let (Some(implementation), Some(method)) = (
            implementation_keys.get(edge.src.as_str()),
            by_id.get(edge.dst.as_str()),
        ) else {
            continue;
        };
        if method.kind != acdd_index::models::tags::NodeKind::Method {
            continue;
        }
        let signature = serde_json::json!({
            "name": method.name,
            "is_static": method.details["is_static"],
            "unproven_bounds": method.details["unproven_bounds"],
            "signature": method.details["signature"],
            "param_types": method.parameter_types_value(),
            "return_type": method.details["return_type"],
        });
        shapes
            .get_mut(implementation)
            .expect("implementation shape was initialized")
            .insert(serde_json::to_string(&signature)?, method.name.clone());
    }
    Ok(shapes)
}

pub(super) fn changed_rust_impl_methods(
    conn: &Connection,
    changed_rust_paths: &BTreeSet<String>,
    facts: &BTreeMap<String, TypedFacts>,
    old_modified_facts: &BTreeMap<String, TypedFacts>,
) -> Result<BTreeSet<String>> {
    if changed_rust_paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    let missing_paths: Vec<&str> = changed_rust_paths
        .iter()
        .filter(|path| !old_modified_facts.contains_key(*path))
        .map(String::as_str)
        .collect();
    let fetched_old: BTreeMap<String, TypedFacts> = if missing_paths.is_empty() {
        BTreeMap::new()
    } else {
        populate_temp_text_frontier(conn, "delta_frontier_text", missing_paths)?;
        let mut st = conn.prepare(
            "SELECT path,facts_blob FROM local_facts WHERE path IN(SELECT key FROM temp.delta_frontier_text)",
        )?;
        let raw_blobs = st
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw_blobs
            .into_par_iter()
            .map(|(path, blob)| Ok((path, decode_facts(&blob)?)))
            .collect::<Result<BTreeMap<_, _>>>()?
    };
    let mut changed_methods = BTreeSet::new();
    for path in changed_rust_paths {
        let old_shapes = old_modified_facts
            .get(path)
            .or_else(|| fetched_old.get(path))
            .map(|typed| rust_impl_shapes(&typed.facts))
            .transpose()?
            .unwrap_or_default();
        let new_shapes = facts
            .get(path)
            .map(|typed| rust_impl_shapes(&typed.facts))
            .transpose()?
            .unwrap_or_default();
        if old_shapes == new_shapes {
            continue;
        }
        for key in old_shapes
            .keys()
            .chain(new_shapes.keys())
            .collect::<BTreeSet<_>>()
        {
            if old_shapes.get(key) == new_shapes.get(key) {
                continue;
            }
            changed_methods.extend(
                old_shapes
                    .get(key)
                    .into_iter()
                    .chain(new_shapes.get(key))
                    .flat_map(|methods| methods.values().cloned()),
            );
        }
    }
    Ok(changed_methods)
}

pub(super) fn route_include_closure(
    conn: &Connection,
    facts: &BTreeMap<String, TypedFacts>,
    modified: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
) -> Result<BTreeSet<String>> {
    let mut module_paths = std::collections::HashMap::new();
    {
        let mut statement = conn.prepare(
            "SELECT owner.path,n.qualname FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE n.kind='module' AND n.language='python'",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (path, module) = row?;
            if modified.contains(&path) || inventory.contains_key(path.as_str()) {
                module_paths.insert((config::workspace_path(&path).0.to_owned(), module), path);
            }
        }
    }
    for (path, typed) in facts {
        let workspace = config::workspace_path(path).0.to_owned();
        for module in typed
            .facts
            .nodes
            .iter()
            .filter(|node| node.kind == acdd_index::models::tags::NodeKind::Module && node.language == acdd_index::models::tags::LanguageId::Python)
        {
            module_paths.insert((workspace.clone(), module.qualname.clone()), path.clone());
        }
    }

    let mut include_rows = Vec::new();
    {
        let mut statement = conn.prepare(
            "SELECT owner.path,n.details FROM nodes n JOIN path_dictionary owner ON owner.path_id=n.owner_path_id WHERE n.kind='route' AND json_extract(n.details,'$.include_module') IS NOT NULL",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (owner, details) = row?;
            if modified.contains(&owner) || !inventory.contains_key(owner.as_str()) {
                continue;
            }
            let details: serde_json::Value = serde_json::from_str(&details)?;
            if let Some(module) = details["include_module"].as_str() {
                include_rows.push((owner, module.to_owned()));
            }
        }
    }
    for (path, typed) in facts {
        for route in typed.facts.nodes.iter().filter(|node| node.kind == acdd_index::models::tags::NodeKind::Route) {
            if let Some(module) = route.details["include_module"].as_str() {
                include_rows.push((path.clone(), module.to_owned()));
            }
        }
    }

    let mut relations = BTreeSet::new();
    for (parent, module) in include_rows {
        let workspace = config::workspace_path(&parent).0.to_owned();
        if let Some(child) = module_paths.get(&(workspace, module)) {
            relations.insert((parent, child.clone()));
        }
    }
    let mut closure = modified.clone();
    loop {
        let mut changed = false;
        for (parent, child) in &relations {
            if closure.contains(parent) || closure.contains(child) {
                changed |= closure.insert(parent.clone());
                changed |= closure.insert(child.clone());
            }
        }
        if !changed {
            break;
        }
    }
    Ok(closure)
}

pub(super) fn semantic_fact_owners(
    conn: &Connection,
    facts: &BTreeMap<String, TypedFacts>,
    seeds: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for (_, typed) in facts.iter().filter(|(path, _)| seeds.contains(*path)) {
        for (_, entry) in typed.facts.semantic.entries() {
            let acdd_index::typed_facts::SemanticState::Valid { facts: values, .. } = &entry.state
            else {
                if let acdd_index::typed_facts::SemanticState::Invalid { error, .. } = &entry.state
                {
                    anyhow::bail!("{error}");
                }
                unreachable!();
            };
            for fact in values {
                keys.insert(languages::semantic::dependency_key(&fact.identity));
            }
        }
        if !typed.facts.semantic.is_complete() {
            for (index, node) in typed.facts.nodes.iter().enumerate() {
                if typed.facts.semantic.state(index).is_some() {
                    continue;
                }
                for fact in languages::semantic::decode(node)? {
                    keys.insert(languages::semantic::dependency_key(&fact.identity));
                }
            }
        }
    }
    if !seeds.is_empty() {
        populate_temp_text_frontier(
            conn,
            "delta_frontier_text",
            seeds.iter().map(String::as_str),
        )?;
        let mut old = conn.prepare("SELECT k.key FROM shared_owners s JOIN shared_keys k ON k.key_hash=s.key_hash JOIN path_dictionary p ON p.path_id=s.owner_id WHERE s.kind_id=1 AND p.path IN(SELECT key FROM temp.delta_frontier_text) AND typeof(k.key)='text' AND k.key LIKE 'semantic:%'")?;
        for key in old.query_map([], |row| row.get::<_, String>(0))? {
            keys.insert(key?);
        }
    }
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    populate_temp_text_frontier(conn, "delta_frontier_text", keys.iter().map(String::as_str))?;
    let mut owners = conn.prepare("SELECT DISTINCT p.path FROM shared_owners s JOIN shared_keys k ON k.key_hash=s.key_hash JOIN path_dictionary p ON p.path_id=s.owner_id WHERE s.kind_id=1 AND k.key IN(SELECT key FROM temp.delta_frontier_text)")?;
    let paths = owners
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(paths
        .into_iter()
        .filter(|path| inventory.contains_key(path.as_str()))
        .collect())
}

pub(super) fn named_projection_candidates(
    conn: &Connection,
    facts: &BTreeMap<String, TypedFacts>,
    seeds: &BTreeSet<String>,
    inventory: &BTreeMap<&str, &FileEntry>,
    dependencies: &languages::dependency_registry::DependencyRegistry,
    changed_resolution: &BTreeSet<String>,
    previous_owners: Option<&BTreeSet<String>>,
) -> Result<BTreeSet<String>> {
    let mut selectors = BTreeSet::new();
    let mut unavailable_scopes = BTreeSet::new();
    let mut unavailable_owners = BTreeSet::new();
    let mut collect = |path: &str, file: &TypedFacts| {
        if file.flows.named_uncertainty_faults().next().is_some() {
            unavailable_scopes.insert(languages::dependency_registry::named_binding_scope(
                path,
                dependencies,
            ));
            unavailable_owners.insert(path.to_owned());
        }
        for reference in file.references.iter().filter(|reference| {
            reference.kind == "semantic_named_reference"
                || (reference.kind == "semantic_named_definition"
                    && changed_resolution.contains(path))
        }) {
            if let Some(domain) = &reference.module {
                selectors.insert((
                    path.to_owned(),
                    domain.clone(),
                    reference.expression.clone(),
                    reference.kind == "semantic_named_definition",
                ));
            }
        }
    };
    for (path, file) in facts.iter().filter(|(path, _)| seeds.contains(*path)) {
        collect(path, file);
    }
    // Only modified/deleted inputs need an old snapshot comparison. Unchanged
    // affected providers contribute selectors after their one canonical load.
    if let Some(previous_owners) = previous_owners.filter(|owners| !owners.is_empty()) {
        populate_temp_text_frontier(
            conn,
            "delta_frontier_text",
            previous_owners.iter().map(String::as_str),
        )?;
        let mut old = conn.prepare(
            "SELECT path,facts_blob FROM local_facts WHERE path IN(SELECT key FROM temp.delta_frontier_text)",
        )?;
        let raw_blobs = old
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let decoded: Vec<(String, TypedFacts)> = raw_blobs
            .into_par_iter()
            .map(|(path, blob)| Ok((path, decode_facts(&blob)?)))
            .collect::<Result<_>>()?;
        for (path, typed) in &decoded {
            collect(path, typed);
        }
    }
    let mut selected = BTreeSet::new();
    // A bounded carrier can explicitly make configured named authority unavailable
    // for its project without enumerating every affected domain. Revisit that same
    // scope on both appearance and removal, including consumers previously unknown.
    if !unavailable_scopes.is_empty() {
        for path in inventory.keys() {
            let admitted = unavailable_scopes
                .contains(&languages::dependency_registry::named_binding_scope(
                    path,
                    dependencies,
                ))
                || dependencies.language_context_for_path(path).syntax_rules.iter().any(|rule| {
                    matches!(&rule.operation, languages::manifests::syntax::SyntaxOperation::NamedBinding { domain, .. }
                        if unavailable_owners.iter().any(|owner| dependencies.named_scope_admits(path, owner, domain)))
                });
            if admitted {
                selected.insert((*path).to_owned());
            }
        }
    }
    let mut query = conn.prepare("SELECT DISTINCT p.path FROM shared_owners domain JOIN shared_keys dk ON dk.key_hash=domain.key_hash JOIN path_dictionary p ON p.path_id=domain.owner_id WHERE domain.kind_id=1 AND domain.key_hash=?1 AND dk.key=?2 AND (?3='' OR EXISTS(SELECT 1 FROM shared_owners key_owner JOIN shared_keys kk ON kk.key_hash=key_owner.key_hash WHERE key_owner.owner_id=domain.owner_id AND key_owner.kind_id=1 AND key_owner.key_hash IN(?4,?5) AND kk.key IN(?3,'')))")?;
    for (owner, domain, key, definition) in selectors {
        for row in query.query_map(
            params![
                stable_hash64(&domain),
                domain,
                key,
                stable_hash64(&key),
                stable_hash64("")
            ],
            |row| row.get::<_, String>(0),
        )? {
            let path = row?;
            if inventory.contains_key(path.as_str())
                && if definition {
                    dependencies.named_scope_admits(&path, &owner, &domain)
                } else {
                    dependencies.named_scope_admits(&owner, &path, &domain)
                }
            {
                selected.insert(path);
            }
        }
    }
    Ok(selected)
}

pub(super) fn persisted_possible_call_owners(
    conn: &Connection,
    changed_rust_paths: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if changed_rust_paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    populate_temp_text_frontier(
        conn,
        "delta_frontier_text",
        changed_rust_paths.iter().map(String::as_str),
    )?;
    let mut owners = BTreeSet::new();
    let mut statement = conn.prepare(
        "SELECT DISTINCT owner.path FROM edge_occurrences candidate JOIN nodes target ON target.node_hash=candidate.dst_hash JOIN path_dictionary target_path ON target_path.path_id=target.path_id JOIN path_dictionary owner ON owner.path_id=candidate.owner_id WHERE candidate.kind='possible_calls' AND target_path.path IN(SELECT key FROM temp.delta_frontier_text)",
    )?;
    for row in statement.query_map([], |row| row.get::<_, String>(0))? {
        owners.insert(row?);
    }
    Ok(owners)
}

pub(super) fn unchanged_trait_dispatch_owners(
    conn: &Connection,
    implementation_methods: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if implementation_methods.is_empty() {
        return Ok(BTreeSet::new());
    }
    populate_temp_text_frontier(
        conn,
        "delta_frontier_text",
        implementation_methods.iter().map(String::as_str),
    )?;
    let mut owners = BTreeSet::new();
    let mut statement = conn.prepare(
        "SELECT DISTINCT owner.path FROM edge_occurrences call JOIN nodes declaration ON declaration.node_hash=call.dst_hash JOIN path_dictionary owner ON owner.path_id=call.owner_id WHERE call.kind='calls' AND declaration.kind='method' AND declaration.name IN(SELECT key FROM temp.delta_frontier_text) AND EXISTS(SELECT 1 FROM resolution_coverage coverage JOIN coverage_expressions expression ON expression.expression_id=coverage.expression_id WHERE coverage.path_id=call.owner_id AND coverage.line=call.line AND expression.expression='trait_object_dispatch')",
    )?;
    for row in statement.query_map([], |row| row.get::<_, String>(0))? {
        owners.insert(row?);
    }
    Ok(owners)
}

pub(super) fn contract_mentions_import(
    value: &Value,
    imported_names: &BTreeSet<String>,
    qualified_aliases: &BTreeSet<String>,
) -> bool {
    fn qualified_alias_matches(value: &str, aliases: &BTreeSet<String>) -> bool {
        aliases.iter().any(|alias| {
            value == alias
                || value
                    .strip_prefix(alias)
                    .and_then(|suffix| suffix.strip_prefix('.'))
                    .is_some_and(|member| !member.is_empty())
        })
    }

    match value {
        Value::String(value) => {
            imported_names.contains(value) || qualified_alias_matches(value, qualified_aliases)
        }
        Value::Array(values) => values
            .iter()
            .any(|value| contract_mentions_import(value, imported_names, qualified_aliases)),
        Value::Object(values) => {
            if values
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "unknown")
            {
                return true;
            }
            if values
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "field")
            {
                let Some(receiver) = values.get("receiver").and_then(Value::as_str) else {
                    return true;
                };
                return qualified_alias_matches(receiver, qualified_aliases)
                    || !imported_names.contains(receiver);
            }
            if let Some(opaque_value) = values.get("opaque_value") {
                return opaque_value.is_null()
                    || contract_mentions_import(opaque_value, imported_names, qualified_aliases);
            }
            if values.keys().any(|key| key.starts_with("opaque")) || values.is_empty() {
                return true;
            }
            values
                .values()
                .any(|value| contract_mentions_import(value, imported_names, qualified_aliases))
        }
        _ => false,
    }
}

#[derive(Default)]
pub(super) struct ImportTargetKinds {
    pub(super) module: bool,
    pub(super) non_module: bool,
    pub(super) unknown: bool,
}

pub(super) fn module_exposes_provider_contract(
    conn: &Connection,
    owner: &str,
    upstream: &str,
    facts: &mut BTreeMap<String, TypedFacts>,
    loaded_fact_files: &mut usize,
) -> Result<bool> {
    if !facts.contains_key(owner) {
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT facts_blob FROM local_facts WHERE path=?1",
                [owner],
                |row| row.get(0),
            )
            .optional()?;
        let Some(blob) = blob else {
            return Ok(true);
        };
        facts.insert(owner.to_owned(), decode_facts(&blob)?);
        *loaded_fact_files += 1;
    }
    let typed = facts.get(owner).expect("module facts were loaded");
    let Some(module) = typed.facts.nodes.iter().find(|node| node.kind == acdd_index::models::tags::NodeKind::Module) else {
        return Ok(true);
    };
    let mut statement = conn.prepare(
        "SELECT DISTINCT edge.line,evidence.evidence,target.kind FROM edge_occurrences edge JOIN nodes target ON target.node_hash=edge.dst_hash JOIN path_dictionary owner ON owner.path_id=edge.owner_id JOIN path_dictionary target_path ON target_path.path_id=target.path_id JOIN coverage_evidence evidence ON evidence.evidence_id=edge.evidence_id WHERE owner.path=?1 AND target_path.path=?2 AND edge.kind='imports'",
    )?;
    let mut import_edges: BTreeMap<(usize, String), ImportTargetKinds> = BTreeMap::new();
    for row in statement.query_map([owner, upstream], |row| {
        Ok((
            row.get::<_, usize>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (line, expression, target_kind) = row?;
        let targets = import_edges.entry((line, expression)).or_default();
        if target_kind == "module" {
            targets.module = true;
        } else if target_kind == "unknown" || target_kind.is_empty() {
            targets.unknown = true;
        } else {
            targets.non_module = true;
        }
    }
    if import_edges.is_empty() {
        return Ok(true);
    }
    let mut imported_names = BTreeSet::new();
    let mut qualified_aliases = BTreeSet::new();
    let mut imported_modules = BTreeSet::new();
    let mut matched_import_edges = BTreeSet::new();
    for reference in typed
        .facts
        .references
        .iter()
        .filter(|reference| reference.kind == "imports" && reference.source == module.id)
    {
        let Some(targets) = import_edges.get(&(reference.line, reference.expression.clone()))
        else {
            continue;
        };
        if targets.unknown || (targets.module && targets.non_module) {
            return Ok(true);
        }
        let Some(import_module) = reference.module.as_deref() else {
            return Ok(true);
        };
        let alias = reference.alias.as_deref().unwrap_or(&reference.expression);
        imported_names.insert(alias.to_owned());
        let is_typescript_namespace_import =
            reference.expression == "*" && alias != "*" && targets.module && !targets.non_module;
        let is_python_module_import =
            module.language == acdd_index::models::tags::LanguageId::Python && targets.module && !targets.non_module;
        if is_typescript_namespace_import || is_python_module_import {
            qualified_aliases.insert(alias.to_owned());
        }
        imported_modules.insert(import_module.to_owned());
        matched_import_edges.insert((reference.line, reference.expression.clone()));
    }
    if matched_import_edges.len() != import_edges.len() {
        return Ok(true);
    }

    let Some(exports) = module.details["exports"].as_array() else {
        return Ok(true);
    };
    let explicit_reexport = exports.iter().any(|export| {
        let export_module = export["module"].as_str();
        let local = export["local"].as_str();
        export_module.is_some_and(|source| imported_modules.contains(source))
            || (export_module.is_none()
                && local.is_some_and(|local| imported_names.contains(local)))
    });
    if explicit_reexport {
        return Ok(true);
    }

    let contracts = linker::contracts::Contracts::new_typed(typed);
    for node in typed
        .facts
        .nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "function" | "method"))
    {
        let exported = if module.language == acdd_index::models::tags::LanguageId::Python {
            !node.name.starts_with('_')
        } else {
            exports.iter().any(|export| {
                export["local"].as_str() == Some(node.name.as_str())
                    || export["name"].as_str() == Some(node.name.as_str())
            })
        };
        if !exported {
            continue;
        }
        let details = contracts.details(node, None);
        let return_contract = &details["value_flow"]["return_contract"];
        if return_contract.is_null()
            || contract_mentions_import(return_contract, &imported_names, &qualified_aliases)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn resolution_identity_changed_facts(old_facts: &TypedFacts, facts: &TypedFacts) -> Result<bool> {
    let old_contracts = linker::contracts::Contracts::new_typed(old_facts);
    let new_contracts = linker::contracts::Contracts::new_typed(facts);
    Ok(old_contracts.resolution_digest(&old_facts.facts)?
        != new_contracts.resolution_digest(&facts.facts)?)
}

