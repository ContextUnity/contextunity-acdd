//! Symbol inspection, explanation, search, source previews, and test discovery queries.

mod inspect;
mod preview;
mod search;
mod test_discovery;
mod types;

#[cfg(test)]
pub(crate) use inspect::snippet_paged;
pub use inspect::{
    attach_inspect_document_contents, explain_with_path_options, inspect_with_path_options,
    snippet_paged_with_response_budget,
};
pub(crate) use inspect::{explain_paged_response, inspect_paged_response};
pub use search::{
    search_results_with_navigation, search_signature_hits, search_with_options,
    SearchSignatureHit,
};
pub(crate) use test_discovery::verified_calls_covered_cte;
pub use test_discovery::{tests_paged_with_path, verified_calls_covered_hashes};
pub use types::{InspectOptions, SearchOptions, SymbolExplainOptions};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        commitments::{write_metadata, MetadataKey},
        reader::{self, ExplainDirection},
        schema::initialize_code_map_schema,
    };
    use acdd_core::response::{
        CoverageOptions, Detail, QueryOptions, ResponsePolicy, SourceOptions,
    };
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
        let source =
            SourceOptions::resolve(&ResponsePolicy::default(), Some(false), None, None, 0).unwrap();

        let explained = explain_with_path_options(
            &conn,
            Path::new("."),
            "alpha",
            Some("src/lib.rs"),
            &SymbolExplainOptions {
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
                classification: None,
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

    #[test]
    fn snippet_paged_chunks_source_lines_with_generation_guard() {
        let dir = acdd_core::fs::ScopedTempDir::create("acdd-ut-snippet-paged").unwrap();
        let root = dir.path();
        let code: String = std::iter::once("def target():\n".to_owned())
            .chain((1..=60).map(|idx| format!("    x_{idx} = {idx}\n")))
            .collect();
        std::fs::write(root.join("code.py"), &code).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(&conn, MetadataKey::OutputRoot, "gen1").unwrap();
        write_metadata(
            &conn,
            MetadataKey::WorkspaceRoot,
            &root.canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO path_dictionary(path_id, path) VALUES (1, 'code.py')",
            [],
        )
        .unwrap();
        let digest = acdd_index_digest(&code);
        conn.execute(
            "INSERT INTO files(path, status, digest, size, language, is_test, generated, path_hash) VALUES ('code.py', 'indexed', ?1, 0, 'python', 0, 0, ?2)",
            rusqlite::params![digest, crate::models::stable_hash64("code.py")],
        ).unwrap();
        let h = crate::models::stable_hash64("py:code.py:1:target");
        conn.execute(
            "INSERT INTO nodes(node_hash, id, kind, name, qualname, path_id, owner_path_id, line, end_line, language, generated, details, is_test)
             VALUES (?1,'py:code.py:1:target','function','target','code.target',1,1,1,61,'python',0,'{\"signature\":\"def target()\"}',0)",
            [h],
        ).unwrap();
        let policy = ResponsePolicy::default();
        let opts =
            QueryOptions::resolve(&policy, Some(30), 0, Some(Detail::Compact), None).unwrap();
        let source = SourceOptions::resolve(&policy, Some(true), Some(0), Some(35), 0).unwrap();
        let first = snippet_paged(&conn, root, "target", &source, &opts).unwrap();
        let offset = first["source_preview"]["next_source_offset"]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(offset, 35);
        let next_source =
            SourceOptions::resolve(&policy, Some(true), Some(0), Some(35), offset).unwrap();
        assert!(snippet_paged(&conn, root, "target", &next_source, &opts).is_err());
        let mut pinned = opts.clone();
        pinned.generation = Some("gen1".to_owned());
        let second = snippet_paged(&conn, root, "target", &next_source, &pinned).unwrap();
        assert_eq!(second["source_preview"]["has_more"], false);
        assert_eq!(
            format!(
                "{}{}",
                first["source"].as_str().unwrap(),
                second["source"].as_str().unwrap()
            ),
            code
        );
        pinned.generation = Some("stale".to_owned());
        assert!(snippet_paged(&conn, root, "target", &next_source, &pinned).is_err());
    }

    fn acdd_index_digest(source: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(source.as_bytes());
        format!("{:x}", h.finalize())
    }

    #[test]
    fn out_of_bounds_symbol_boundary_rejects_inspect_explain_and_select() {
        let dir = acdd_core::fs::ScopedTempDir::create("acdd-ut-oob-boundary").unwrap();
        let root = dir.path();
        let source = "def resilient_hit(): return 1\n# no declaration\n";
        std::fs::write(root.join("a.py"), source).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        initialize_code_map_schema(&conn).unwrap();
        write_metadata(&conn, MetadataKey::OutputRoot, "gen1").unwrap();
        write_metadata(
            &conn,
            MetadataKey::WorkspaceRoot,
            &root.canonicalize().unwrap().to_string_lossy(),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO path_dictionary(path_id, path) VALUES (1, 'a.py')",
            [],
        )
        .unwrap();
        let digest = acdd_index_digest(source);
        conn.execute(
            "INSERT INTO files(path, status, digest, size, language, is_test, generated, path_hash) VALUES ('a.py', 'indexed', ?1, 0, 'python', 0, 0, ?2)",
            rusqlite::params![digest, crate::models::stable_hash64("a.py")],
        )
        .unwrap();
        let h = crate::models::stable_hash64("py:a.py:2:resilient_hit");
        conn.execute(
            "INSERT INTO nodes(node_hash, id, kind, name, qualname, path_id, owner_path_id, line, end_line, language, generated, details, is_test)
             VALUES (?1,'py:a.py:2:resilient_hit','function','resilient_hit','a.resilient_hit',1,1,2,2,'python',0,'{\"signature\":\"def resilient_hit()\"}',0)",
            [h],
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
        let source_opts =
            SourceOptions::resolve(&ResponsePolicy::default(), Some(true), None, None, 0).unwrap();
        let expected = "indexed symbol boundary is unavailable";

        let inspect_err = inspect_with_path_options(
            &conn,
            root,
            "a.py:resilient_hit",
            None,
            &InspectOptions {
                show_doc: false,
                source: &source_opts,
                coverage: CoverageOptions::default(),
                page: &page,
            },
        )
        .unwrap_err();
        assert!(
            inspect_err.to_string().contains(expected),
            "{inspect_err:#}"
        );

        let explain_err = explain_with_path_options(
            &conn,
            root,
            "a.py:resilient_hit",
            None,
            &SymbolExplainOptions {
                explain: reader::ExplainOptions {
                    direction: ExplainDirection::Both,
                    show_doc: false,
                    page: &page,
                },
                source: &source_opts,
                coverage: CoverageOptions::default(),
            },
        )
        .unwrap_err();
        assert!(
            explain_err.to_string().contains(expected),
            "{explain_err:#}"
        );

        let select_err =
            snippet_paged(&conn, root, "a.py:resilient_hit", &source_opts, &page).unwrap_err();
        assert!(select_err.to_string().contains(expected), "{select_err:#}");
    }
}
