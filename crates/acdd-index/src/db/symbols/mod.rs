//! Symbol inspection, explanation, search, source previews, and test discovery queries.

mod inspect;
mod preview;
mod search;
mod source;
mod test_discovery;

use super::{paging, reader};
use acdd_core::response::{CoverageOptions, QueryOptions, SourceOptions};

pub use inspect::{
    attach_inspect_document_contents, explain_with_path_options, inspect_with_path_options,
    snippet_paged, snippet_paged_with_response_budget,
};
pub(crate) use inspect::{explain_paged_response, inspect_paged_response};
pub use search::{search_results_with_navigation, search_with_options};
pub(crate) use source::{read_confined_source_bytes, verified_source, VerifiedSourceCache};
pub use test_discovery::tests_paged_with_path;

/// Filters and paging for a symbol search.
pub struct SearchOptions<'a> {
    /// Optional symbol kind.
    pub kind: Option<&'a str>,
    /// Workspace-relative file or directory scope.
    pub path: Option<&'a str>,
    /// Whether Markdown document symbols are included.
    pub include_docs: bool,
    /// Whether the name or qualified name must match exactly.
    pub exact: bool,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Output controls for symbol inspection.
pub struct InspectOptions<'a> {
    /// Whether linked documentation is included.
    pub show_doc: bool,
    /// Optional bounded source preview.
    pub source: &'a SourceOptions,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
    /// Bounded page and generation contract.
    pub page: &'a QueryOptions,
}

/// Output controls for symbol explanation.
pub struct ExplainOptions<'a> {
    /// Core explanation direction, documentation inclusion, and paging contract.
    pub explain: reader::ExplainOptions<'a>,
    /// Optional bounded source preview.
    pub source: &'a SourceOptions,
    /// Optional resolution coverage.
    pub coverage: CoverageOptions,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        commitments::{write_metadata, MetadataKey},
        initialize_code_map_schema,
        reader::ExplainDirection,
    };
    use acdd_core::response::{Detail, ResponsePolicy};
    use rusqlite::Connection;
    use std::path::Path;

    #[test]
    fn explain_options_compose_direction_and_search_filters_work_in_memory() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(&conn, MetadataKey::OutputRoot, "gen1").unwrap();
        conn.execute(
            "INSERT INTO path_dictionary(path_id, path) VALUES (1, 'src/lib.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO coverage_evidence(evidence_id, evidence) VALUES (1, 'exact')",
            [],
        )
        .unwrap();
        let h_a = crate::models::stable_hash64("fn:src/lib.rs:1:alpha");
        let h_b = crate::models::stable_hash64("fn:src/lib.rs:5:beta");
        conn.execute(
            "INSERT INTO nodes(node_hash, id, kind, name, qualname, path_id, owner_path_id, line, end_line, language, generated, details, is_test)
             VALUES (?1,'fn:src/lib.rs:1:alpha','function','alpha','lib.alpha',1,1,1,3,'rust',0,'{\"signature\":\"fn alpha()\"}',0),
                    (?2,'fn:src/lib.rs:5:beta','function','beta','lib.beta',1,1,5,7,'rust',0,'{\"signature\":\"fn beta()\"}',0)",
            rusqlite::params![h_a, h_b],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges(src_hash, dst_hash, kind, path_id, line, evidence_id, confidence_id, occurrence_count)
             VALUES (?1, ?2, 'calls', 1, 2, 1, 1, 1)",
            rusqlite::params![h_a, h_b],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO node_search(rowid, search_text)
             VALUES (?1, 'alpha lib.alpha src/lib.rs'),
                    (?2, 'beta lib.beta src/lib.rs')",
            rusqlite::params![h_a, h_b],
        )
        .unwrap();

        let page = QueryOptions::resolve(
            &ResponsePolicy::default(),
            Some(10),
            0,
            Some(Detail::Compact),
            None,
        )
        .unwrap();
        let source = SourceOptions::resolve(&ResponsePolicy::default(), Some(false), None, None, 0)
            .unwrap();

        let explained = explain_with_path_options(
            &conn,
            Path::new("."),
            "alpha",
            Some("src/lib.rs"),
            &ExplainOptions {
                explain: reader::ExplainOptions {
                    direction: ExplainDirection::Outgoing,
                    show_doc: false,
                    page: &page,
                },
                source: &source,
                coverage: CoverageOptions::default(),
            },
        )
        .unwrap();
        assert_eq!(explained["direction"], "outgoing");
        assert_eq!(explained["outgoing"]["total"], 1);
        assert_eq!(explained["incoming"]["omitted"], true);

        let searched = search_with_options(
            &conn,
            "alpha",
            &SearchOptions {
                kind: Some("function"),
                path: Some("file://./src"),
                include_docs: false,
                exact: true,
                page: &page,
            },
        )
        .unwrap();
        assert_eq!(searched["nodes"]["total"], 1);
        assert_eq!(searched["nodes"]["items"][0]["name"], "alpha");
    }
}
