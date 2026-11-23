//! Read-only code-map SQLite admission, query execution, selectors, and projections.

mod diagnostics;
pub(crate) mod docs;
pub(crate) mod inspect;
mod open;
mod search;
pub(crate) mod selectors;
pub(crate) mod sql;
pub(crate) mod types;

pub use diagnostics::analyze_paged;
pub use docs::{get_doc_paged, search_docs_with_options};
pub use inspect::{governing_invariants, governing_invariants_for_scope, inspect_paged_with_path};
pub use open::{
    inventory_snapshot, open, open_locked, open_raw_confined_readonly, open_with_diagnostics,
    read_policy, validate_database_header, validate_database_integrity, validate_workspace,
    LockedConnection,
};
pub use search::{documentation_hubs, overview_with_options};
pub use selectors::{directory_path, indexed_path_suggestions, path_bounds};
pub(crate) use selectors::{select_detail_with_path, strip_selector_uri_prefix};
pub use sql::rows;
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use sql::rows_with_mode;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        commitments::{write_metadata, MetadataKey, MAX_QUERY_RESULT_BYTES},
        schema::initialize_code_map_schema,
        paging::{self, RowMode},
    };
    use acdd_core::response::{Detail, QueryOptions, ResponsePolicy};
    use rusqlite::Connection;
    use serde_json::{json, Value};

    fn page(limit: usize, offset: usize, generation: Option<&str>) -> QueryOptions {
        QueryOptions::resolve(&ResponsePolicy::default(), Some(limit), offset, Some(Detail::Compact), generation.map(str::to_owned)).unwrap()
    }

    #[test]
    fn overview_aspect_and_explain_direction_contract_matrix() {
        assert_eq!(OverviewAspect::ALL.len(), 15);
        for aspect in OverviewAspect::ALL {
            assert_eq!(aspect.as_str().parse::<OverviewAspect>().unwrap(), aspect);
            assert_eq!(aspect.as_str().to_ascii_uppercase().parse::<OverviewAspect>().unwrap(), aspect);
            assert_eq!(aspect.is_discovery(), DISCOVERY_OVERVIEW_ASPECTS.contains(&aspect.as_str()));
        }
        let err = "bogus_aspect".parse::<OverviewAspect>().unwrap_err().to_string();
        assert!(err.contains("unsupported overview aspect 'bogus_aspect'") && OverviewAspect::ALL.iter().all(|a| err.contains(a.as_str())));
        assert_eq!(ExplainDirection::parse_optional(None).unwrap(), ExplainDirection::Both);
        for (raw, exp, inc, out, wire) in [
            ("both", ExplainDirection::Both, true, true, "both"),
            ("incoming", ExplainDirection::Incoming, true, false, "incoming"),
            ("inbound", ExplainDirection::Incoming, true, false, "incoming"),
            ("outgoing", ExplainDirection::Outgoing, false, true, "outgoing"),
            ("outbound", ExplainDirection::Outgoing, false, true, "outgoing"),
        ] {
            let parsed = ExplainDirection::parse_optional(Some(raw)).unwrap();
            assert_eq!((parsed, parsed.includes_incoming(), parsed.includes_outgoing(), parsed.as_str()), (exp, inc, out, wire));
        }
        assert!(ExplainDirection::parse_optional(Some("sideways")).is_err());
    }

    #[test]
    fn rows_with_mode_incremental_byte_accounting_matches_serde_and_enforces_8mib() {
        let conn = Connection::open_in_memory().unwrap();
        let sql = "SELECT NULL n,0 z,-42 neg,9223372036854775807 mxi,-9223372036854775808 mni,3.25 f,'plain' s1,'q\"\\'||char(10)||'🦀' s2,'{\"k\":[1,2]}' details,'raw' invariants,X'00ff10' b,1 dup,2 dup,999 node_hash UNION ALL SELECT 1,2,3,4,5,0.5,'b','c','{\"x\":true}','p',X'abcd',10,20,888";
        for mode in [RowMode::Structural, RowMode::Opaque] {
            let (res, bytes) = sql::rows_with_mode_and_bytes(&conn, sql, &[], 10, mode).unwrap();
            assert_eq!(bytes, serde_json::to_vec(&res).unwrap().len());
            assert!(bytes <= MAX_QUERY_RESULT_BYTES);
            assert_eq!(res[0]["dup"], 2);
            assert_eq!(res[0].get("node_hash").is_none(), mode == RowMode::Structural);
            assert_eq!(res[0]["details"].is_object(), mode == RowMode::Structural);
        }
        let big = "x".repeat(3 * 1024 * 1024);
        let err = rows_with_mode(&conn, "SELECT ?1 a UNION ALL SELECT ?1||'1' UNION ALL SELECT ?1||'2'", &[&big], 10, RowMode::Opaque).unwrap_err().to_string();
        assert!(err.contains("8 MiB row budget"), "{err}");
    }

    #[test]
    fn paging_query_single_pass_handles_empty_ordered_and_past_end_pages() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(&conn, MetadataKey::OutputRoot, "gen-test-0001").unwrap();
        conn.execute_batch("CREATE TABLE t_items(id INTEGER PRIMARY KEY,label TEXT);INSERT INTO t_items VALUES(1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');").unwrap();
        let empty = paging::query(&conn, "SELECT id FROM t_items WHERE id<0", &[], &page(2, 0, None)).unwrap();
        assert_eq!((&empty["total"], &empty["has_more"], &empty["items"]), (&json!(0), &json!(false), &json!([])));
        let cte = "WITH ordered AS (SELECT id,label FROM t_items ORDER BY id DESC) SELECT id,label FROM ordered";
        let p0 = paging::query(&conn, cte, &[], &page(2, 0, None)).unwrap();
        assert_eq!((&p0["total"], &p0["has_more"], &p0["next_offset"], p0["items"][0]["id"].as_i64()), (&json!(5), &json!(true), &json!(2), Some(5)));
        let past_end = paging::query(&conn, cte, &[], &page(2, 10, p0["generation"].as_str())).unwrap();
        assert_eq!((&past_end["total"], &past_end["has_more"], &past_end["items"]), (&json!(5), &json!(false), &Value::Array(Vec::new())));
    }
}
