use super::super::commitments::{read_metadata_optional, MetadataKey};
use super::super::paging;
use super::selectors::path_bounds;
use super::sql::rows;
use super::types::DocSearchOptions;
use crate::models::tags::EdgeKind;
use acdd_core::response::{Detail, MAX_PAGE_SIZE, QueryOptions};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Fetches document contents by their document identifiers.
pub fn document_contents_by_ids(
    conn: &Connection,
    doc_ids: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>> {
    if doc_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    ensure!(
        doc_ids.len() <= MAX_PAGE_SIZE,
        "document content lookup exceeds the inspected page bound"
    );
    let placeholders = (1..=doc_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT doc_id,content FROM doc_sections WHERE doc_id IN ({placeholders}) ORDER BY doc_id"
    );
    let params = doc_ids
        .iter()
        .map(|doc_id| doc_id as &dyn ToSql)
        .collect::<Vec<_>>();
    let rows = rows(conn, &sql, &params, doc_ids.len())?;
    let mut contents = BTreeMap::new();
    for row in rows {
        let doc_id = row
            .get("doc_id")
            .and_then(Value::as_str)
            .context("document content lookup returned a row without doc_id")?;
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .context("document content lookup returned a row without content")?;
        contents.insert(doc_id.to_owned(), content.to_owned());
    }
    ensure!(
        contents.len() == doc_ids.len(),
        "document content lookup could not resolve every inspected section"
    );
    Ok(contents)
}

/// Resolves component prefix.
fn resolve_component_prefix(conn: &Connection, component: &str) -> String {
    let comp = component.trim().trim_end_matches('/');
    if comp.is_empty() {
        return String::new();
    }
    // 1. Direct path check: if any doc begins with `comp/` or equals `comp`, use it as is
    let direct_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM doc_sections WHERE path=?1 OR (path>=?2 AND path<?3))",
            rusqlite::params![comp, format!("{comp}/"), format!("{comp}0")],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if direct_exists {
        return comp.to_string();
    }

    // 2. Check metadata adapter JSON for aliases and owners
    if let Ok(Some(raw)) = read_metadata_optional(conn, MetadataKey::Adapter) {
        if let Ok(val) = serde_json::from_str::<Value>(&raw) {
            let aliases = val.get("aliases").and_then(|v| v.as_object());
            let owners = val.get("owners").and_then(|v| v.as_object());

            // Target name: either alias resolution or component itself
            let target_owner = aliases
                .and_then(|a| a.get(comp))
                .and_then(|v| v.as_str())
                .unwrap_or(comp);

            // Find matching path in owners
            if let Some(owners_map) = owners {
                for (owner_path, owner_name) in owners_map {
                    if let Some(name_str) = owner_name.as_str() {
                        if name_str.eq_ignore_ascii_case(target_owner)
                            || name_str.eq_ignore_ascii_case(comp)
                        {
                            return owner_path.trim_end_matches('/').to_string();
                        }
                    }
                }
                // Also check if comp matches an owner path directly or ends with /{comp}
                for owner_path in owners_map.keys() {
                    if owner_path.eq_ignore_ascii_case(comp)
                        || owner_path.ends_with(&format!("/{comp}"))
                    {
                        return owner_path.trim_end_matches('/').to_string();
                    }
                }
            }
        }
    }

    // 3. Fallback: check if any doc_sections path contains `/{comp}/`
    let pattern = format!("%/{comp}/%");
    if let Ok(matched_path) = conn.query_row::<String, _, _>(
        "SELECT path FROM doc_sections WHERE path LIKE ?1 ORDER BY path LIMIT 1",
        [&pattern],
        |r| r.get(0),
    ) {
        if let Some((prefix, _)) = matched_path.split_once(&format!("/{comp}/")) {
            return format!("{prefix}/{comp}");
        }
    }

    comp.to_string()
}

/// Search indexed documentation using one bounded options contract.
pub fn search_docs_with_options(
    conn: &Connection,
    query: &str,
    request: &DocSearchOptions<'_>,
) -> Result<Value> {
    let doc_type = request.doc_type;
    let component = request.component;
    let include_excerpt = request.include_excerpt;
    let options = request.page;
    if query.trim().is_empty() {
        bail!("search query is empty");
    }
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .filter(|term| {
            !DOC_SEARCH_ENGLISH_STOP_WORDS
                .iter()
                .any(|stop_word| term.eq_ignore_ascii_case(stop_word))
        })
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        let sql = format!(
            "SELECT {} FROM doc_sections d WHERE 0",
            paging::docs("d", options.detail)
        );
        return Ok(json!({"sections":paging::query(conn, &sql, &[], options)?}));
    }
    let query = terms.join(" OR ");
    let kind = doc_type.unwrap_or("");
    let resolved = resolve_component_prefix(conn, component.unwrap_or(""));
    let component = resolved.as_str();
    let archive_filter = if component == "archive"
        || component.starts_with("archive/")
        || component.ends_with("/archive")
        || component.contains("/archive/")
    {
        ""
    } else {
        " AND d.path NOT LIKE 'archive/%' AND d.path NOT LIKE '%/archive/%'"
    };
    let (prefix, end) = path_bounds(component);
    let excerpt = if include_excerpt {
        ",substr(snippet(doc_search,-1,'[',']',' … ',16),1,240) excerpt"
    } else {
        ""
    };
    let and_query = terms.join(" AND ");
    let (cte, order) = if terms.len() > 1 {
        (
            "WITH fts_and_matches AS (SELECT rowid FROM doc_search WHERE doc_search MATCH ?6) ",
            "CASE WHEN doc_search.rowid IN (SELECT rowid FROM fts_and_matches) THEN 0 ELSE 1 END,rank,d.path,d.doc_id",
        )
    } else {
        ("", "rank,d.path,d.doc_id")
    };
    let sql = format!(
        "{cte}SELECT {},bm25(doc_search,3.0,1.0,1.0) rank{excerpt} FROM doc_search JOIN doc_sections d ON d.rowid=doc_search.rowid WHERE doc_search MATCH ?1 AND (?2='' OR d.doc_type=?2) AND (?3='' OR d.path=?3 OR (d.path>=?4 AND d.path<?5)){archive_filter} ORDER BY {order}",
        paging::docs("d", options.detail)
    );
    let mut params: Vec<&dyn ToSql> = vec![&query, &kind, &component, &prefix, &end];
    if terms.len() > 1 {
        params.push(&and_query);
    }
    Ok(json!({"sections":paging::query(conn, &sql, &params, options)?}))
}

const DOC_SEARCH_ENGLISH_STOP_WORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "against", "all", "am", "an", "and", "any", "are",
    "as", "at", "be", "because", "been", "before", "being", "below", "between", "both", "but",
    "by", "can", "could", "did", "do", "does", "doing", "down", "during", "each", "few", "for",
    "from", "further", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "more", "most", "my", "no",
    "nor", "not", "now", "of", "off", "on", "once", "only", "or", "other", "our", "out", "over",
    "own", "same", "she", "should", "so", "some", "such", "than", "that", "the", "their", "them",
    "then", "there", "these", "they", "this", "those", "through", "to", "too", "under", "until",
    "up", "very", "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom",
    "why", "will", "with", "would", "you", "your",
];

const DOCUMENT_REFERENCE_CANDIDATES_SQL: &str = r#"
WITH source_claims AS MATERIALIZED (
    SELECT
        CAST(claim.key AS INTEGER) AS claim_index,
        json_extract(claim.value,'$.doc_id') AS source_doc_id,
        json_extract(claim.value,'$.source_path') AS source_path,
        json_extract(claim.value,'$.name') AS claim_name,
        json_extract(claim.value,'$.normalized_name') AS normalized_name,
        CAST(json_extract(claim.value,'$.line') AS INTEGER) AS source_line,
        CAST(json_extract(claim.value,'$.explicit_path') AS INTEGER) AS explicit_path,
        json_extract(claim.value,'$.target_path') AS claim_target_path,
        CAST(json_extract(claim.value,'$.suffix_indexed') AS INTEGER) AS suffix_indexed
    FROM json_each(?1) claim
),
fts_targets AS MATERIALIZED (
    SELECT n.node_id,n.id,n.name,n.qualname,n.kind,p.path,n.line
    FROM node_search
    JOIN nodes n ON n.node_hash=node_search.rowid
    JOIN path_dictionary p ON p.path_id=n.path_id
    WHERE node_search MATCH ?2
      AND n.kind NOT IN('document','invariant','component')
      AND (
        ((?3='' AND NOT (
            substr(p.path,1,1)='['
            AND instr(p.path,'/')>1
            AND substr(p.path,instr(p.path,'/')-1,1)=']'
        )) OR (?3!='' AND substr(p.path,1,length(?3))=?3
            AND substr(p.path,length(?3)+1,1)='/'))
        OR EXISTS (
            SELECT 1 FROM source_claims explicit_claim
            WHERE explicit_claim.explicit_path=1
              AND explicit_claim.claim_target_path IS NOT NULL
              AND explicit_claim.claim_target_path=p.path
        )
      )
),
candidate_rows AS (
    SELECT c.claim_index,c.source_doc_id,c.source_path,c.claim_name,c.source_line,
           c.explicit_path,c.claim_target_path,n.node_id AS target_node_id,n.id AS target_id,
           n.name AS target_name,n.qualname AS target_qualname,n.kind AS target_kind,
           p.path AS target_path,n.line AS target_line,0 AS priority
    FROM source_claims c
    JOIN nodes n INDEXED BY idx_nodes_qualname ON n.qualname=c.normalized_name
    JOIN path_dictionary p ON p.path_id=n.path_id
    UNION ALL
    SELECT c.claim_index,c.source_doc_id,c.source_path,c.claim_name,c.source_line,
           c.explicit_path,c.claim_target_path,n.node_id,n.id,n.name,n.qualname,n.kind,
           p.path,n.line,1
    FROM source_claims c
    JOIN nodes n INDEXED BY idx_nodes_name ON n.name=c.claim_name
    JOIN path_dictionary p ON p.path_id=n.path_id
    UNION ALL
    SELECT c.claim_index,c.source_doc_id,c.source_path,c.claim_name,c.source_line,
           c.explicit_path,c.claim_target_path,n.node_id,n.id,n.name,n.qualname,n.kind,
           n.path,n.line,2
    FROM source_claims c
    JOIN fts_targets n
      ON c.suffix_indexed=1
     AND length(c.normalized_name)>0
     AND length(n.qualname)>length(c.normalized_name)
     AND substr(n.qualname,-length(c.normalized_name))=c.normalized_name
     AND substr(n.qualname,length(n.qualname)-length(c.normalized_name),1)='.'
    UNION ALL
    /*TOKENLESS_SUFFIX_FALLBACK*/
),
eligible_candidates AS (
    SELECT *
    FROM candidate_rows
    WHERE target_kind NOT IN('document','invariant','component')
      AND (
        (explicit_path=1 AND claim_target_path IS NOT NULL AND target_path=claim_target_path)
        OR
        (explicit_path=0 AND (
            (?3='' AND NOT (
                substr(target_path,1,1)='['
                AND instr(target_path,'/')>1
                AND substr(target_path,instr(target_path,'/')-1,1)=']'
            ))
            OR
            (?3!='' AND substr(target_path,1,length(?3))=?3
                AND substr(target_path,length(?3)+1,1)='/')
        ))
      )
),
ranked_candidates AS (
    SELECT *,MIN(priority) OVER(
        PARTITION BY claim_index,source_doc_id,source_path,explicit_path,claim_target_path
    ) AS selected_priority
    FROM eligible_candidates
),
winning_candidates AS (
    SELECT * FROM ranked_candidates WHERE priority=selected_priority
),
ambiguous_claims AS (
    SELECT claim_index,source_doc_id,source_path,explicit_path,claim_target_path
    FROM winning_candidates
    GROUP BY claim_index,source_doc_id,source_path,explicit_path,claim_target_path
    HAVING COUNT(DISTINCT target_node_id)>1
)
SELECT DISTINCT
    winner.source_doc_id,winner.source_path,winner.claim_name,winner.source_line,
    winner.target_id,winner.target_name,winner.target_qualname,winner.target_kind,
    winner.target_path,winner.target_line
FROM winning_candidates winner
JOIN ambiguous_claims ambiguous
  ON ambiguous.claim_index=winner.claim_index
 AND ambiguous.source_doc_id=winner.source_doc_id
 AND ambiguous.source_path=winner.source_path
 AND ambiguous.explicit_path=winner.explicit_path
 AND ambiguous.claim_target_path IS winner.claim_target_path
ORDER BY winner.source_path,winner.source_line,winner.source_doc_id,winner.claim_name,
         winner.target_path,winner.target_line,winner.target_id
"#;

fn document_reference_candidate_page(
    conn: &Connection,
    selected_sections: &[Value],
    source_workspace: &str,
    options: &QueryOptions,
) -> Result<Value> {
    let mut claims = Vec::new();
    let mut fts_terms = BTreeSet::new();
    let mut has_tokenless_claim = false;
    for section in selected_sections {
        let (Some(doc_id), Some(source_path)) =
            (section["doc_id"].as_str(), section["path"].as_str())
        else {
            continue;
        };
        let detailed_claims = section["details"]["symbol_claims"]
            .as_array()
            .filter(|claims| !claims.is_empty());
        let source_line = section["node_line"].as_u64().unwrap_or(1);
        let mut append_claim =
            |name: &str, line: u64, explicit_path: bool, target_path: Option<&str>| {
                let normalized_name = name.replace("::", ".");
                let suffix_indexed = normalized_name.chars().any(char::is_alphanumeric);
                if suffix_indexed {
                    fts_terms.insert(normalized_name.clone());
                } else {
                    has_tokenless_claim = true;
                }
                claims.push(json!({
                    "doc_id":doc_id,
                    "source_path":source_path,
                    "name":name,
                    "normalized_name":normalized_name,
                    "line":line,
                    "explicit_path":explicit_path,
                    "target_path":target_path,
                    "suffix_indexed":suffix_indexed,
                }));
            };
        if let Some(detailed_claims) = detailed_claims {
            for claim in detailed_claims {
                if let Some(name) = claim["name"].as_str() {
                    append_claim(
                        name,
                        claim["line"].as_u64().unwrap_or(source_line),
                        claim["explicit_path"].as_bool().unwrap_or(false),
                        claim["target_path"].as_str(),
                    );
                }
            }
        } else if let Some(fallback_claims) = section["referenced_symbols"].as_array() {
            for name in fallback_claims.iter().filter_map(Value::as_str) {
                append_claim(name, source_line, false, None);
            }
        }
    }

    if claims.is_empty() {
        let generation = paging::generation(conn, options)?;
        return Ok(paging::value(Vec::new(), 0, options, &generation));
    }
    let claims_json = serde_json::to_string(&claims)?;
    let fts_query = if fts_terms.is_empty() {
        "\"__acdd_document_reference_suffix_no_tokens__\"".to_owned()
    } else {
        fts_terms
            .iter()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let tokenless_fallback = if has_tokenless_claim {
        "SELECT c.claim_index,c.source_doc_id,c.source_path,c.claim_name,c.source_line,\n           c.explicit_path,c.claim_target_path,n.node_id,n.id,n.name,n.qualname,n.kind,\n           p.path,n.line,2\n    FROM source_claims c\n    JOIN nodes n INDEXED BY idx_nodes_qualname\n      ON c.suffix_indexed=0\n     AND length(c.normalized_name)>0\n     AND length(n.qualname)>length(c.normalized_name)\n     AND substr(n.qualname,-length(c.normalized_name))=c.normalized_name\n     AND substr(n.qualname,length(n.qualname)-length(c.normalized_name),1)='.'\n    JOIN path_dictionary p ON p.path_id=n.path_id"
    } else {
        "SELECT NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,2 WHERE 0"
    };
    let sql = DOCUMENT_REFERENCE_CANDIDATES_SQL
        .replace("/*TOKENLESS_SUFFIX_FALLBACK*/", tokenless_fallback);
    let mut page = paging::query(
        conn,
        &sql,
        &[&claims_json, &fts_query, &source_workspace],
        options,
    )?;
    let items = page["items"].as_array().cloned().unwrap_or_default();
    page["items"] = Value::Array(
        items
            .into_iter()
            .map(|item| {
                json!({
                    "source":{
                        "doc_id":item["source_doc_id"],
                        "path":item["source_path"],
                        "name":item["claim_name"],
                        "line":item["source_line"],
                    },
                    "target":{
                        "id":item["target_id"],
                        "name":item["target_name"],
                        "qualname":item["target_qualname"],
                        "kind":item["target_kind"],
                        "path":item["target_path"],
                        "line":item["target_line"],
                    },
                })
            })
            .collect(),
    );
    Ok(page)
}

/// Returns the doc paged.
pub fn get_doc_paged(
    conn: &Connection,
    path: &str,
    section: Option<&str>,
    options: &QueryOptions,
) -> Result<Value> {
    let section = section.unwrap_or("");
    let status_projection = if options.detail == Detail::Full {
        ",json_extract(n.details,'$.status') AS status"
    } else {
        ""
    };
    let sections = paging::query(
        conn,
        &format!(
            "SELECT {},n.line,n.end_line{status_projection} FROM doc_sections d LEFT JOIN nodes n ON n.id=d.doc_id WHERE (d.doc_id=?1 OR d.path=?1) AND (?2='' OR d.section_title=?2) ORDER BY d.path,d.rowid",
            paging::docs("d", options.detail)
        ),
        &[&path, &section],
        options,
    )?;
    if sections["total"] == 0 {
        bail!("document or section not found");
    }
    const MAX_RELATION_SCOPE_SECTIONS: usize = 1000;
    let relation_scope_limit = (MAX_RELATION_SCOPE_SECTIONS + 1) as i64;
    let relation_scope_sql = if options.detail == Detail::Full {
        "SELECT d.doc_id,d.path,n.details,n.line AS node_line,d.referenced_symbols FROM doc_sections d JOIN nodes n ON n.id=d.doc_id WHERE (d.doc_id=?1 OR d.path=?1) AND (?2='' OR d.section_title=?2) ORDER BY d.path,d.rowid LIMIT ?3"
    } else {
        "SELECT d.doc_id,d.path FROM doc_sections d WHERE (d.doc_id=?1 OR d.path=?1) AND (?2='' OR d.section_title=?2) ORDER BY d.path,d.rowid LIMIT ?3"
    };
    let selected_sections = rows(
        conn,
        relation_scope_sql,
        &[&path, &section, &relation_scope_limit],
        MAX_RELATION_SCOPE_SECTIONS + 1,
    )?;
    if selected_sections.len() > MAX_RELATION_SCOPE_SECTIONS {
        bail!("document relation scope exceeds 1000 sections; narrow the section selector");
    }
    let hashes = selected_sections
        .iter()
        .filter_map(|item| item["doc_id"].as_str())
        .map(crate::models::stable_hash64)
        .collect::<Vec<_>>();
    let source_workspace = selected_sections
        .first()
        .and_then(|item| item["path"].as_str())
        .map(|path| acdd_core::config::workspace_path(path).0.to_owned())
        .unwrap_or_default();
    let encoded_hashes = serde_json::to_string(&hashes)?;
    let doc_edge_query = |kind: &str| {
        paging::query(
            conn,
            &format!(
                "SELECT d.doc_id,d.path,d.section_title,d.doc_type,e.line FROM edges e JOIN nodes n ON n.node_hash=e.dst_hash JOIN doc_sections d ON d.doc_id=n.id WHERE e.kind='{kind}' AND e.src_hash IN(SELECT CAST(value AS INTEGER) FROM json_each(?1)) ORDER BY d.path,d.doc_id,e.line"
            ),
            &[&encoded_hashes],
            options,
        )
    };
    let cites_doc = doc_edge_query("cites_doc")?;
    let cited_by_doc = doc_edge_query("cited_by_doc")?;
    let referenced_symbols = paging::query(
        conn,
        &format!(
            "SELECT source.id AS doc_id,symbol.id,symbol.name,symbol.kind,symbol.path_id,(SELECT path FROM path_dictionary p WHERE p.path_id=symbol.path_id) AS path,symbol.line,e.line AS citation_line FROM edges e JOIN nodes source ON source.node_hash=e.src_hash JOIN doc_sections d ON d.doc_id=source.id JOIN nodes symbol ON symbol.node_hash=e.dst_hash WHERE e.kind='{}' AND e.src_hash IN(SELECT CAST(value AS INTEGER) FROM json_each(?1)) ORDER BY d.path,d.doc_id,e.line,symbol.path_id,symbol.line,symbol.id",
            EdgeKind::Documents.as_str(),
        ),
        &[&encoded_hashes],
        options,
    )?;
    let referenced_symbol_candidates = if options.detail == Detail::Full {
        document_reference_candidate_page(conn, &selected_sections, &source_workspace, options)?
    } else {
        json!({"omitted":true,"hint":"Request detail=full for documentary reference candidates"})
    };
    Ok(json!({"sections":sections,"cites_doc":cites_doc,"cited_by_doc":cited_by_doc,"referenced_symbols":referenced_symbols,"referenced_symbol_candidates":referenced_symbol_candidates}))
}
