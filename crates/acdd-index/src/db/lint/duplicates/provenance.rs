use crate::models::{
    details::NodeDetailKey,
    tags::{CoverageStatus, EdgeKind, NodeKind},
};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;

fn strip_trait_generics(raw: &str) -> Option<&str> {
    let base = raw
        .trim()
        .split('<')
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_start_matches("::");
    (!base.is_empty()).then_some(base)
}

fn strip_std_root(path: &str) -> Option<&str> {
    path.strip_prefix("std::")
        .or_else(|| path.strip_prefix("core::"))
        .or_else(|| path.strip_prefix("alloc::"))
}

fn is_std_root(path: &str) -> bool {
    matches!(path, "std" | "core" | "alloc") || strip_std_root(path).is_some()
}

fn is_standard_mapper_qualified(path: &str) -> bool {
    matches!(
        path,
        "fmt::Display"
            | "fmt::Debug"
            | "convert::From"
            | "convert::TryFrom"
            | "convert::Into"
            | "convert::TryInto"
            | "convert::AsRef"
            | "convert::AsMut"
            | "ops::Deref"
            | "ops::DerefMut"
            | "default::Default"
            | "clone::Clone"
            | "cmp::PartialEq"
            | "cmp::Eq"
            | "cmp::PartialOrd"
            | "cmp::Ord"
            | "hash::Hash"
            | "borrow::Borrow"
            | "borrow::BorrowMut"
            | "string::ToString"
            | "str::FromStr"
    )
}

fn is_prelude_mapper_trait(name: &str) -> bool {
    matches!(
        name,
        "From"
            | "TryFrom"
            | "Into"
            | "TryInto"
            | "AsRef"
            | "AsMut"
            | "Default"
            | "Clone"
            | "PartialEq"
            | "Eq"
            | "PartialOrd"
            | "Ord"
            | "ToString"
    )
}

fn parse_external_import_target(evidence: &str) -> Option<String> {
    let rest = evidence.strip_prefix("reference through external import ")?;
    let head = rest.split(';').next()?.trim();
    let target = match head.rsplit_once(" at line ") {
        Some((prefix, line_part))
            if !line_part.is_empty() && line_part.bytes().all(|b| b.is_ascii_digit()) =>
        {
            prefix.trim()
        }
        _ => head,
    };
    let normalized = target.replace('.', "::");
    let trimmed = normalized.trim_start_matches("::");
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn has_positive_standard_trait_provenance(
    base_trait: &str,
    evidence: &str,
    path_shadowed: bool,
) -> bool {
    if path_shadowed
        || base_trait.starts_with("self::")
        || base_trait.starts_with("super::")
        || base_trait.starts_with("crate::")
    {
        return false;
    }
    if let Some(import_target) = parse_external_import_target(evidence) {
        if !is_std_root(&import_target) {
            return false;
        }
        if let Some(std_rel) = strip_std_root(base_trait) {
            return is_standard_mapper_qualified(std_rel);
        }
        if let Some((_, tail)) = base_trait.split_once("::") {
            let combined = format!("{import_target}::{tail}");
            return strip_std_root(&combined).is_some_and(is_standard_mapper_qualified);
        }
        return strip_std_root(&import_target).is_some_and(is_standard_mapper_qualified);
    }
    if is_prelude_mapper_trait(base_trait) {
        return evidence
            .split_once("standard library prelude:")
            .is_some_and(|(_, tail)| tail.trim() == base_trait);
    }
    false
}

pub(super) fn load_shadowed_paths(conn: &Connection) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT rc.path_id
         FROM resolution_coverage rc
         JOIN coverage_expressions ce ON ce.expression_id = rc.expression_id
         WHERE ce.expression = 'macro_expansion_not_analyzed'
         UNION
         SELECT DISTINCT e.path_id
         FROM edges e
         JOIN nodes s ON s.node_hash = e.src_hash
         JOIN nodes d ON d.node_hash = e.dst_hash
         WHERE e.kind = '{calls}'
           AND s.kind IN ('{module}', '{module_decl}')
           AND d.kind = '{macro_kind}'
         UNION
         SELECT DISTINCT d.owner_id
         FROM dependencies d
         WHERE d.kind = '{imports}'
           AND (d.symbol = '*' OR d.symbol LIKE '%::*' OR d.symbol LIKE '%.*')
         UNION
         SELECT DISTINCT n.path_id
         FROM nodes n
         WHERE n.name IN ('std', 'core', 'alloc')
           AND n.kind IN ('{module}', '{module_decl}', 'import')
         UNION
         SELECT DISTINCT d.owner_id
         FROM dependencies d
         WHERE d.kind = '{imports}'
           AND (
               d.symbol IN ('std', 'core', 'alloc')
               OR d.symbol LIKE '%::std' OR d.symbol LIKE '%::core' OR d.symbol LIKE '%::alloc'
               OR d.symbol LIKE '%.std' OR d.symbol LIKE '%.core' OR d.symbol LIKE '%.alloc'
           )",
        calls = EdgeKind::Calls.as_str(),
        module = NodeKind::Module.as_str(),
        module_decl = NodeKind::ModuleDeclaration.as_str(),
        macro_kind = NodeKind::Macro.as_str(),
        imports = EdgeKind::Imports.as_str(),
    ))?;
    let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
    let mut paths = HashSet::new();
    for path_id in rows {
        paths.insert(path_id?);
    }
    Ok(paths)
}

pub(super) fn load_standard_mapper_method_ids(
    conn: &Connection,
    shadowed_paths: &HashSet<i64>,
) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT m.id,
                owner.path_id,
                json_extract(owner.details, '$.{impl_trait}'),
                COALESCE(rc.status, ''),
                COALESCE(ce.expression, ''),
                COALESCE(ev.evidence, '')
         FROM nodes m
         JOIN edges e_own ON e_own.dst_hash = m.node_hash AND e_own.kind = '{contains}'
         JOIN nodes owner ON owner.node_hash = e_own.src_hash AND owner.kind = '{impl_kind}'
         LEFT JOIN resolution_coverage rc
           ON rc.path_id = owner.path_id
          AND rc.line = owner.line
         LEFT JOIN coverage_expressions ce ON ce.expression_id = rc.expression_id
         LEFT JOIN coverage_evidence ev ON ev.evidence_id = rc.evidence_id
         WHERE m.kind = '{method}'
           AND json_extract(owner.details, '$.{impl_trait}') IS NOT NULL
           AND NOT EXISTS (
               SELECT 1
               FROM edges e_impl
               WHERE e_impl.kind = '{implements}'
                 AND (e_impl.src_hash = m.node_hash OR e_impl.src_hash = owner.node_hash)
           )",
        impl_trait = NodeDetailKey::IMPL_TRAIT,
        contains = EdgeKind::Contains.as_str(),
        impl_kind = NodeKind::Impl.as_str(),
        method = NodeKind::Method.as_str(),
        implements = EdgeKind::Implements.as_str(),
    ))?;
    let external_status = CoverageStatus::External.as_str();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut approved = HashSet::new();
    for entry in rows {
        let (method_id, owner_path_id, raw_trait, status, expression, evidence) = entry?;
        let Some(base_trait) = strip_trait_generics(&raw_trait) else {
            continue;
        };
        let path_shadowed = shadowed_paths.contains(&owner_path_id);
        if path_shadowed {
            continue;
        }
        if status != external_status {
            continue;
        }
        let Some(expr_base) = strip_trait_generics(&expression) else {
            continue;
        };
        if expr_base != base_trait {
            continue;
        }
        if has_positive_standard_trait_provenance(base_trait, &evidence, path_shadowed) {
            approved.insert(method_id);
        }
    }
    Ok(approved)
}

pub(super) fn is_trivial_trait_mapper(is_standard_mapper_impl: bool, stmt_len: usize) -> bool {
    is_standard_mapper_impl && stmt_len <= 6
}
