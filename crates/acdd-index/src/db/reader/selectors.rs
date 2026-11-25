use super::super::paging;
use super::sql::rows;
use super::types::{AmbiguousCandidate, SelectorError};
use crate::models::details::{NodeDetailFlags, NodeDetailKey};
use crate::models::tags::NodeKind;
use acdd_core::response::Detail;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

/// Strips `file://`, `file:`, and leading `./` prefixes from a code or path selector.
pub(crate) fn strip_selector_uri_prefix(selector: &str) -> &str {
    let unprefixed = selector
        .strip_prefix("file://")
        .or_else(|| selector.strip_prefix("file:"))
        .unwrap_or(selector);
    unprefixed.strip_prefix("./").unwrap_or(unprefixed)
}

/// SQLite BINARY ordering makes [path + '/', path + '0') a literal descendant range.
pub fn path_bounds(path: &str) -> (String, String) {
    let path = path.trim_end_matches('/');
    (format!("{path}/"), format!("{path}0"))
}

/// Directory selectors are resolved from indexed paths. A suffix is accepted only when it
/// identifies one directory; otherwise the caller must supply a workspace-relative path.
pub fn directory_path(conn: &Connection, selector: &str) -> Result<Option<String>> {
    let selector = strip_selector_uri_prefix(selector.trim()).trim_end_matches('/');
    if selector.is_empty()
        || selector.starts_with('/')
        || selector
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(SelectorError::InvalidSyntax {
            selector: selector.to_owned(),
            reason: "directory path must be workspace-relative without empty, '.' or '..' segments"
                .into(),
        }
        .into());
    }
    let (lower, upper) = path_bounds(selector);
    let exact: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE path>=?1 AND path<?2)",
        [&lower, &upper],
        |row| row.get(0),
    )?;
    if exact {
        return Ok(Some(selector.to_owned()));
    }
    let needle = format!("/{selector}/");
    let mut statement = conn.prepare(
        "SELECT DISTINCT substr(path,1,instr(path,?1)+length(?1)-2) AS directory \
         FROM files WHERE instr(path,?1)>0 ORDER BY directory LIMIT 11",
    )?;
    let candidates = statement
        .query_map([&needle], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match candidates.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => Err(SelectorError::AmbiguousPaths {
            selector: selector.to_owned(),
            candidates,
        }
        .into()),
    }
}

/// Suggests indexed paths that match or contain the given selector prefix.
pub fn indexed_path_suggestions(conn: &Connection, selector: &str) -> Result<Vec<String>> {
    let name = selector
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if name.len() < 2 {
        return Ok(Vec::new());
    }
    let mut statement =
        conn.prepare("SELECT path FROM files WHERE instr(path,?1)>0 ORDER BY path LIMIT 5")?;
    let suggestions = statement
        .query_map([name], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(suggestions)
}

fn is_code_path(s: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        ".py", ".rs", ".ts", ".tsx", ".js", ".jsx", ".vue", ".go", ".c", ".cpp", ".cc", ".cxx",
        ".h", ".hpp", ".cs", ".java", ".kt", ".rb", ".php", ".proto", ".json", ".yaml", ".yml",
        ".toml", ".sql", ".sh", ".bash",
    ];
    EXTENSIONS.iter().any(|ext| s.ends_with(ext))
}

fn is_known_kind_name(kind: &str) -> bool {
    kind == "fn"
        || matches!(
            NodeKind::parse(kind),
            Some(
                NodeKind::Function
                    | NodeKind::Class
                    | NodeKind::Module
                    | NodeKind::Struct
                    | NodeKind::Method
                    | NodeKind::Trait
                    | NodeKind::Interface
                    | NodeKind::Type
                    | NodeKind::Component
            )
        )
}

fn normalize_scope_path(path: Option<&str>, selector: &str) -> Result<String> {
    let Some(path) = path else {
        return Ok(String::new());
    };
    let raw_path = path.trim();
    let path = raw_path.replace('\\', "/");
    let invalid = raw_path.is_empty()
        || path.starts_with('/')
        || path.as_bytes().contains(&0)
        || path.split('/').any(|part| part == "..")
        || path.split_once(':').is_some_and(|(drive, _)| {
            drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic()
        });
    if invalid {
        return Err(SelectorError::InvalidSyntax {
            selector: selector.to_owned(),
            reason: "path must be workspace-relative and cannot contain parent traversal".into(),
        }
        .into());
    }
    Ok(path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/"))
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn ambiguous_candidates(conn: &Connection, ids: &[String]) -> Result<Vec<AmbiguousCandidate>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(ids)?;
    rows(
        conn,
        "SELECT n.id,n.qualname,p.path,n.line,n.details FROM json_each(?1) requested JOIN nodes n ON n.id=requested.value JOIN path_dictionary p ON p.path_id=n.path_id ORDER BY n.id LIMIT 101",
        &[&ids],
        101,
    )?
    .into_iter()
    .map(|row| {
        let details = match &row["details"] {
            Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
            other => other.clone(),
        };
        Ok(AmbiguousCandidate {
            id: row["id"].as_str().context("candidate id is missing")?.to_owned(),
            path: row["path"].as_str().context("candidate path is missing")?.to_owned(),
            line: row["line"].as_i64().unwrap_or(0),
            qualname: row["qualname"].as_str().unwrap_or_default().to_owned(),
            signature: details
                .get(NodeDetailKey::SIGNATURE)
                .and_then(Value::as_str)
                .map(|signature| bounded_chars(signature, 512))
                .unwrap_or_default(),
        })
    })
    .collect()
}

pub(crate) fn select_detail_with_path(
    conn: &Connection,
    selector: &str,
    path: Option<&str>,
    detail: Detail,
) -> Result<Value> {
    let scope = normalize_scope_path(path, selector)?;
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(SelectorError::InvalidSyntax {
            selector: selector.to_owned(),
            reason: "use an exact node id or code_map_search to find symbols".into(),
        }
        .into());
    }
    let unpeeled = strip_selector_uri_prefix(trimmed);
    let (normalized, target_line) = parse_line_target(unpeeled);
    if normalized.ends_with(".md") {
        return Err(SelectorError::DocLink {
            target: normalized.to_owned(),
        }
        .into());
    }
    if let Some(line) = target_line {
        return select_line_anchor(conn, selector, normalized, line, &scope, detail);
    }

    let exact_id = rows(
        conn,
        "SELECT n.id, n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id=?1 AND (?2='' OR p.path=?2 OR substr(p.path,1,length(?2)+1)=?2||'/') LIMIT 1",
        &[&normalized, &scope],
        1,
    )?;
    if let Some(hit) = exact_id.first() {
        let id = hit["id"].as_str().context("invalid node id")?;
        return load_selected_node(conn, id, detail);
    }

    let (mut result, path_qualified) = query_selector_candidates(conn, normalized, &scope)?;
    result = apply_suffix_fallback(conn, normalized, &scope, path_qualified, result)?;
    if result.is_empty() && !path_qualified && !scope.is_empty() {
        result = query_imported_symbol_candidates(conn, normalized, &scope)?;
    }
    result = disambiguate_selector_candidates(conn, normalized, result)?;

    match result.len() {
        0 => Err(SelectorError::NotFound {
            selector: selector.to_owned(),
            suggestions: Vec::new(),
        }
        .into()),
        1 => {
            let id = result[0]["id"].as_str().context("invalid node id")?;
            load_selected_node(conn, id, detail)
        }
        _ => {
            let ids = result
                .iter()
                .filter_map(|value| value["id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            Err(SelectorError::Ambiguous {
                selector: selector.to_owned(),
                candidates: ambiguous_candidates(conn, &ids)?,
            }
            .into())
        }
    }
}

fn parse_line_target(unpeeled: &str) -> (&str, Option<i64>) {
    let (clean_selector, line_anchor) = if let Some((base, hash_part)) = unpeeled.split_once('#') {
        let line_str = hash_part.trim_start_matches('L');
        if let Ok(l) = line_str.parse::<i64>() {
            (base, Some(l))
        } else {
            (unpeeled, None)
        }
    } else {
        (unpeeled, None)
    };
    if let Some(l) = line_anchor {
        return (clean_selector, Some(l));
    }
    if let Some((base, num_part)) = clean_selector.rsplit_once(':') {
        if base.contains('/') || base.contains('\\') || is_code_path(base) {
            if let Ok(l) = num_part.parse::<i64>() {
                return (base, Some(l));
            }
        }
    }
    (clean_selector, None)
}

fn split_kind_and_dotted(normalized: &str) -> (&str, String) {
    if let Some((kind, rest)) = normalized.split_once(':') {
        if !rest.starts_with(':') && is_known_kind_name(kind) {
            let canonical_kind = if kind == "fn" { "function" } else { kind };
            return (canonical_kind, rest.replace("::", "."));
        }
    }
    ("", normalized.replace("::", "."))
}

fn query_path_qualified_candidates(
    conn: &Connection,
    prefix: &str,
    symbol_part: &str,
    scope: &str,
) -> Result<Vec<Value>> {
    let (path_kind, symbol) = split_kind_and_dotted(symbol_part);
    let mut candidates = rows(
        conn,
        "SELECT n.id,n.kind FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE (p.path=?1 OR substr(p.path,1,length(?1)+1)=?1||'/') AND ((n.name=?2 COLLATE NOCASE AND n.name=?2 COLLATE BINARY) OR (n.qualname=?2 COLLATE NOCASE AND n.qualname=?2 COLLATE BINARY)) AND (?3='' OR n.kind=?3) AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') ORDER BY n.id LIMIT 101",
        &[&prefix, &symbol, &path_kind, &scope],
        101,
    )?;
    if candidates.is_empty() {
        candidates = rows(
            conn,
            "SELECT n.id,n.kind FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE substr(p.path,-length(?1))=?1 AND (length(p.path)=length(?1) OR substr(p.path,-length(?1)-1,1)='/') AND ((n.name=?2 COLLATE NOCASE AND n.name=?2 COLLATE BINARY) OR (n.qualname=?2 COLLATE NOCASE AND n.qualname=?2 COLLATE BINARY)) AND (?3='' OR n.kind=?3) AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') ORDER BY n.id LIMIT 101",
            &[&prefix, &symbol, &path_kind, &scope],
            101,
        )?;
    }
    if candidates.is_empty() {
        candidates = rows(
            conn,
            "SELECT n.id,n.kind FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE (p.path=?1 OR substr(p.path,1,length(?1)+1)=?1||'/' OR (substr(p.path,-length(?1))=?1 AND (length(p.path)=length(?1) OR substr(p.path,-length(?1)-1,1)='/'))) AND (n.qualname=?2 OR (length(n.qualname)>length(?2) AND substr(n.qualname,-length(?2))=?2 AND substr(n.qualname,length(n.qualname)-length(?2),1) IN ('.','#'))) AND (?3='' OR n.kind=?3) AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') ORDER BY n.id LIMIT 101",
            &[&prefix, &symbol, &path_kind, &scope],
            101,
        )?;
    }
    let non_modules: Vec<_> = candidates
        .iter()
        .filter(|v| {
            !matches!(
                v["kind"].as_str(),
                Some("module") | Some("file") | Some("component")
            )
        })
        .cloned()
        .collect();
    Ok(if non_modules.is_empty() {
        candidates
    } else {
        non_modules
    })
}

fn query_unqualified_candidates(
    conn: &Connection,
    target: &str,
    normalized: &str,
    scope: &str,
) -> Result<Vec<Value>> {
    let base_name = target.strip_prefix("./").unwrap_or(target);
    rows(
        conn,
        "SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.name=?1 COLLATE NOCASE AND n.name=?1 COLLATE BINARY AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.qualname=?1 COLLATE NOCASE AND n.qualname=?1 COLLATE BINARY AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?2 AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id=?3 AND (?4='' OR p.path=?4 OR substr(p.path,1,length(?4)+1)=?4||'/') ORDER BY id LIMIT 101",
        &[&base_name, &normalized, &format!("module:{normalized}"), &scope],
        101,
    )
}

fn query_selector_candidates(
    conn: &Connection,
    normalized: &str,
    scope: &str,
) -> Result<(Vec<Value>, bool)> {
    let (prefix_part, symbol_part, is_double_colon) =
        if let Some((p, rest)) = normalized.split_once(':') {
            if let Some(s) = rest.strip_prefix(':') {
                (Some(p), s, true)
            } else {
                (Some(p), rest, false)
            }
        } else {
            (None, normalized, false)
        };

    let Some(prefix) = prefix_part else {
        let matches = query_unqualified_candidates(conn, normalized, normalized, scope)?;
        return Ok((matches, false));
    };

    let path_lower = format!("{prefix}/");
    let path_upper = format!("{prefix}0");
    let path_params: [&dyn rusqlite::ToSql; 3] = [&prefix, &path_lower, &path_upper];
    let indexed_path = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM path_dictionary WHERE path=?1 OR (path>=?2 AND path<?3))",
            path_params,
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false);
    let is_path =
        prefix.contains('/') || prefix.contains('\\') || is_code_path(prefix) || indexed_path;
    if is_path {
        let matches = query_path_qualified_candidates(conn, prefix, symbol_part, scope)?;
        return Ok((matches, true));
    }
    if is_double_colon {
        let dotted = normalized.replace("::", ".");
        let matches = query_unqualified_candidates(conn, &dotted, normalized, scope)?;
        return Ok((matches, false));
    }
    if is_known_kind_name(prefix) {
        let kind = if prefix == "fn" { "function" } else { prefix };
        let sym = symbol_part.replace("::", ".");
        let matches = rows(
            conn,
            "SELECT n.id, n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.name=?1 COLLATE NOCASE AND n.name=?1 COLLATE BINARY AND n.kind=?2 AND (?3='' OR p.path=?3 OR substr(p.path,1,length(?3)+1)=?3||'/') UNION SELECT n.id, n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.qualname=?1 COLLATE NOCASE AND n.qualname=?1 COLLATE BINARY AND n.kind=?2 AND (?3='' OR p.path=?3 OR substr(p.path,1,length(?3)+1)=?3||'/') ORDER BY id LIMIT 101",
            &[&sym, &kind, &scope],
            101,
        )?;
        return Ok((matches, false));
    }
    let base_name = symbol_part.strip_prefix("./").unwrap_or(symbol_part);
    let matches = rows(
        conn,
        "SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.name=?1 COLLATE NOCASE AND n.name=?1 COLLATE BINARY AND (?2='' OR n.kind=?2) AND (?5='' OR p.path=?5 OR substr(p.path,1,length(?5)+1)=?5||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.qualname=?1 COLLATE NOCASE AND n.qualname=?1 COLLATE BINARY AND (?2='' OR n.kind=?2) AND (?5='' OR p.path=?5 OR substr(p.path,1,length(?5)+1)=?5||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE p.path=?3 AND (?5='' OR p.path=?5 OR substr(p.path,1,length(?5)+1)=?5||'/') UNION SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id=?4 AND (?5='' OR p.path=?5 OR substr(p.path,1,length(?5)+1)=?5||'/') ORDER BY id LIMIT 101",
        &[&base_name, &prefix, &normalized, &format!("module:{normalized}"), &scope],
        101,
    )?;
    Ok((matches, false))
}

fn apply_suffix_fallback(
    conn: &Connection,
    normalized: &str,
    scope: &str,
    path_qualified: bool,
    result: Vec<Value>,
) -> Result<Vec<Value>> {
    if !result.is_empty() || path_qualified {
        return Ok(result);
    }
    let (kind_filter, clean_target) = split_kind_and_dotted(normalized);
    let suffix_matches = rows(
        conn,
        "SELECT n.id,n.kind FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE (?3='' OR n.kind=?3) AND (n.qualname=?1 OR (length(n.qualname)>length(?1) AND substr(n.qualname,-length(?1))=?1 AND substr(n.qualname,length(n.qualname)-length(?1),1) IN ('.','#')) OR (?3='' AND (p.path=?1 OR (length(p.path)>length(?1) AND substr(p.path,-length(?1))=?1 AND substr(p.path,length(p.path)-length(?1),1)='/')))) AND (?2='' OR p.path=?2 OR substr(p.path,1,length(?2)+1)=?2||'/') ORDER BY n.id LIMIT 101",
        &[&clean_target, &scope, &kind_filter],
        101,
    )?;
    if suffix_matches.len() <= 1 {
        return Ok(suffix_matches);
    }
    let exact_suffix: Vec<_> = suffix_matches
        .iter()
        .filter(|v| {
            v["id"]
                .as_str()
                .is_some_and(|id| id.ends_with(&clean_target))
        })
        .cloned()
        .collect();
    Ok(if exact_suffix.len() == 1 {
        exact_suffix
    } else {
        suffix_matches
    })
}

fn query_imported_symbol_candidates(
    conn: &Connection,
    normalized: &str,
    scope: &str,
) -> Result<Vec<Value>> {
    let (kind_filter, clean_target) = split_kind_and_dotted(normalized);
    rows(
        conn,
        "WITH scoped_nodes(node_hash) AS (\
             SELECT n.node_hash FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id \
             WHERE p.path=?1 OR substr(p.path,1,length(?1)+1)=?1||'/'\
         ), imported_targets(id,kind,name,qualname,path_id) AS (\
             SELECT DISTINCT dst.id,dst.kind,dst.name,dst.qualname,dst.path_id \
             FROM scoped_nodes s \
             JOIN edges e ON e.src_hash=s.node_hash AND e.kind IN ('imports','references') \
             JOIN nodes dst ON dst.node_hash=e.dst_hash\
         ), imported_paths(path_id) AS (\
             SELECT DISTINCT path_id FROM imported_targets WHERE kind IN ('module','file','component')\
         ) \
         SELECT id,kind FROM imported_targets \
         WHERE kind NOT IN ('module','file','component') AND (?3='' OR kind=?3) \
           AND ((name=?2 COLLATE NOCASE AND name=?2 COLLATE BINARY) \
             OR (qualname=?2 COLLATE NOCASE AND qualname=?2 COLLATE BINARY) \
             OR (length(qualname)>length(?2) AND substr(qualname,-length(?2))=?2 AND substr(qualname,length(qualname)-length(?2),1)='.')) \
         UNION \
         SELECT n.id,n.kind FROM imported_paths ip JOIN nodes n ON n.path_id=ip.path_id \
         WHERE n.kind NOT IN ('module','file','component') AND (?3='' OR n.kind=?3) \
           AND ((n.name=?2 COLLATE NOCASE AND n.name=?2 COLLATE BINARY) \
             OR (n.qualname=?2 COLLATE NOCASE AND n.qualname=?2 COLLATE BINARY) \
             OR (length(n.qualname)>length(?2) AND substr(n.qualname,-length(?2))=?2 AND substr(n.qualname,length(n.qualname)-length(?2),1)='.')) \
         ORDER BY id LIMIT 101",
        &[&scope, &clean_target, &kind_filter],
        101,
    )
}

fn disambiguate_selector_candidates(
    conn: &Connection,
    normalized: &str,
    mut result: Vec<Value>,
) -> Result<Vec<Value>> {
    let is_path_like = normalized.contains('/')
        || normalized.contains('\\')
        || is_code_path(normalized)
        || normalized.starts_with("module:")
        || conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
                [&normalized],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false);

    if is_path_like && result.len() > 1 {
        let modules: Vec<_> = result
            .iter()
            .filter(|v| {
                matches!(
                    v["kind"].as_str(),
                    Some("module") | Some("file") | Some("component")
                )
            })
            .cloned()
            .collect();
        if modules.len() == 1 {
            result = modules;
        }
    }

    if result.len() > 1 {
        let has_callable = result
            .iter()
            .any(|v| matches!(v["kind"].as_str(), Some("method" | "function")));
        let has_fields = result.iter().any(|v| v["kind"].as_str() == Some("field"));
        if has_callable && has_fields {
            result.retain(|v| v["kind"].as_str() != Some("field"));
        }
    }

    if result.len() <= 1 {
        return Ok(result);
    }

    let mut enriched_nodes = Vec::with_capacity(result.len());
    for cand in &result {
        let id = cand["id"].as_str().context("invalid candidate node id")?;
        let mut row_vals = rows(
            conn,
            "SELECT n.id,n.kind,n.name,n.qualname,p.path,n.line,n.end_line,n.details FROM nodes n JOIN path_dictionary p ON p.path_id=n.path_id WHERE n.id=?1 LIMIT 1",
            &[&id],
            1,
        )?;
        enriched_nodes.push(
            row_vals
                .pop()
                .context("candidate node disappeared during selector disambiguation")?,
        );
    }

    let non_stubs: Vec<_> = enriched_nodes
        .iter()
        .filter(|v| {
            let details_val = match &v["details"] {
                Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            let flags = NodeDetailFlags::from_value(&details_val);
            !flags.is_stub && !flags.is_overload
        })
        .cloned()
        .collect();
    if !non_stubs.is_empty() && non_stubs.len() < enriched_nodes.len() {
        enriched_nodes = non_stubs;
    }

    if enriched_nodes.len() > 1 {
        let with_body: Vec<_> = enriched_nodes
            .iter()
            .filter(|v| v["end_line"].as_i64().unwrap_or(0) > v["line"].as_i64().unwrap_or(0))
            .cloned()
            .collect();
        if with_body.len() == 1 {
            enriched_nodes = with_body;
        } else if with_body.len() > 1 && same_parent_qualname(&enriched_nodes) {
            if let Some(best) = with_body.into_iter().max_by_key(|v| {
                let line = v["line"].as_i64().unwrap_or(0);
                let end_line = v["end_line"].as_i64().unwrap_or(0);
                (end_line - line, line)
            }) {
                enriched_nodes = vec![best];
            }
        }
    }

    Ok(if enriched_nodes.is_empty() {
        result
    } else {
        enriched_nodes
    })
}

fn same_parent_qualname(nodes: &[Value]) -> bool {
    let first_path = nodes[0]["path"].as_str().unwrap_or("");
    let first_qual_parent = nodes[0]["qualname"]
        .as_str()
        .and_then(|q| q.rsplit_once('.').map(|(p, _)| p))
        .unwrap_or("");
    nodes.iter().all(|n| {
        n["path"].as_str() == Some(first_path)
            && n["qualname"]
                .as_str()
                .and_then(|q| q.rsplit_once('.').map(|(p, _)| p))
                == Some(first_qual_parent)
    })
}

fn select_line_anchor(
    conn: &Connection,
    selector: &str,
    normalized_path: &str,
    line: i64,
    scope: &str,
    detail: Detail,
) -> Result<Value> {
    let exact_path_match = rows(
        conn,
        "SELECT n.id FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE p.path=?1 AND (?3='' OR p.path=?3 OR substr(p.path,1,length(?3)+1)=?3||'/') AND n.line<=?2 AND n.end_line>=?2 ORDER BY CASE WHEN n.kind IN ('module', 'file', 'component') THEN 1 ELSE 0 END ASC, (n.end_line - n.line) ASC, n.id LIMIT 1",
        &[&normalized_path, &line, &scope],
        1,
    )?;
    if let Some(candidate) = exact_path_match.first() {
        let id = candidate["id"].as_str().context("invalid node id")?;
        return load_selected_node(conn, id, detail);
    }

    let exact_file_exists = !rows(
        conn,
        "SELECT path FROM files WHERE path=?1 AND (?2='' OR path=?2 OR substr(path,1,length(?2)+1)=?2||'/') LIMIT 1",
        &[&normalized_path, &scope],
        1,
    )?
    .is_empty();
    if exact_file_exists {
        return Err(SelectorError::NotFound {
            selector: selector.to_owned(),
            suggestions: Vec::new(),
        }
        .into());
    }

    // Suffix matching is a fallback only and must start at a path-component boundary.
    let suffix_paths = rows(
        conn,
        "SELECT DISTINCT p.path FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE substr(p.path,-length(?1))=?1 AND (length(p.path)=length(?1) OR substr(p.path,-length(?1)-1,1)='/') AND (?3='' OR p.path=?3 OR substr(p.path,1,length(?3)+1)=?3||'/') AND n.line<=?2 AND n.end_line>=?2 ORDER BY p.path LIMIT 2",
        &[&normalized_path, &line, &scope],
        2,
    )?;
    if suffix_paths.is_empty() {
        return Err(SelectorError::NotFound {
            selector: selector.to_owned(),
            suggestions: Vec::new(),
        }
        .into());
    }

    let mut candidates = Vec::with_capacity(suffix_paths.len());
    for path_row in &suffix_paths {
        let path = path_row["path"]
            .as_str()
            .context("invalid candidate path")?;
        let best_match = rows(
            conn,
            "SELECT n.id FROM path_dictionary p JOIN nodes n ON n.path_id=p.path_id WHERE p.path=?1 AND n.line<=?2 AND n.end_line>=?2 ORDER BY CASE WHEN n.kind IN ('module', 'file', 'component') THEN 1 ELSE 0 END ASC, (n.end_line - n.line) ASC, n.id LIMIT 1",
            &[&path, &line],
            1,
        )?;
        candidates.push(
            best_match
                .first()
                .and_then(|candidate| candidate["id"].as_str())
                .context("line-anchor path has no matching node")?
                .to_owned(),
        );
    }

    if candidates.len() > 1 {
        return Err(SelectorError::Ambiguous {
            selector: selector.to_owned(),
            candidates: ambiguous_candidates(conn, &candidates)?,
        }
        .into());
    }

    load_selected_node(conn, &candidates[0], detail)
}

fn load_selected_node(conn: &Connection, id: &str, detail: Detail) -> Result<Value> {
    rows(
        conn,
        &format!(
            "SELECT {} FROM nodes n WHERE n.id=?1 LIMIT 1",
            paging::nodes("n", detail)
        ),
        &[&id],
        1,
    )?
    .pop()
    .context("selected node disappeared during selector resolution")
}
