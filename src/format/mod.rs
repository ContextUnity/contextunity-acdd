//! Shared, pure response formatting and presentation compaction.

mod analyze;
mod coordination;
mod core;
mod discovery;
mod lifecycle;
mod lint;
mod presentation;
mod roadmap;
mod traversal;

pub use coordination::{render_blackboard_markdown, render_plugin_tool_markdown};
pub use core::{OperationKind, OutputFormat};
pub use discovery::DiscoveryKind;
pub use lifecycle::{
    render_claim_markdown, render_state_command_markdown, render_state_query_markdown,
    render_submit_markdown, render_verify_markdown,
};
pub(crate) use presentation::{
    attach_document_presentation_uris, attach_lint_presentation_uris, attach_presentation_uris,
    attach_traversal_presentation_uris, present_value_bounded, BoundedPresentation,
    PresentationOptions,
};
pub use presentation::{compact, render_for_operation};
pub use roadmap::{render_roadmap_markdown_with_options, RoadmapRenderOptions};

#[cfg(test)]
mod tests {
    use super::presentation::{render, render_presented};
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_keeps_all_values_and_marks_the_operation() {
        let rendered = render(
            &json!({"count":0,"complete":false,"items":["row",null]}),
            OutputFormat::Markdown,
            "code_map_query result",
        );
        for needle in [
            "# code_map_query result",
            "**count**: `0`",
            "**complete**: `false`",
            "\"row\"",
            "`null`",
        ] {
            assert!(rendered.contains(needle), "missing {needle}: {rendered}");
        }
        let ep = || json!({"total":0,"offset":0,"limit":10,"items":[]});
        let heading_cases = vec![
            (
                "code_map_explain",
                json!({"incoming":ep(),"outgoing":ep(),"documents":ep(),"affected_docs":ep(),"analysis_gaps":ep()}),
                vec![
                    "Inbound relationships",
                    "Outbound relationships",
                    "Verified documentation",
                    "Affected documents",
                    "Analysis gaps",
                ],
            ),
            (
                "code_map_impact",
                json!({"nodes":ep(),"changed_symbols":ep(),"tests":ep(),"unmapped_changes":ep(),"affected_docs":ep()}),
                vec![
                    "Reached nodes",
                    "Changed symbols",
                    "Affected tests",
                    "Unmapped changes",
                    "Affected documents",
                ],
            ),
            (
                "code_map_tests",
                json!({"nodes":ep()}),
                vec!["Related tests and dependencies"],
            ),
            (
                "code_map_analyze",
                json!({"diagnostics":ep(),"untested_public_surface":ep(),"errors":ep(),"resolution":ep(),"external_imports":ep(),"doc_diagnostics":ep(),"top_files":ep(),"resolution_statuses":ep(),"resolution_causes":ep(),"parse_error_languages":ep()}),
                vec![
                    "Syntax diagnostics",
                    "Untested public surface",
                    "Parse errors",
                    "Unresolved and ambiguous references",
                    "External references",
                    "Documentation diagnostics",
                    "Files with the most recorded issues",
                    "Resolution statuses",
                    "Resolution causes",
                    "Parse errors by language",
                ],
            ),
        ];
        let mut failures = Vec::new();
        for (title, value, headings) in heading_cases {
            let md = render_for_operation(
                &value,
                OutputFormat::Markdown,
                title,
                None,
                OperationKind::Navigation,
            );
            for h in headings {
                if md.contains(&format!("## {h}\n")) {
                    failures.push(format!("{title} rendered empty heading {h}"));
                }
            }
            if md.contains("No indexed results in this page.") {
                failures.push(format!("{title} rendered empty notice"));
            }
        }
        let stub = render_for_operation(
            &json!({"incoming":{"total":3,"offset":0,"limit":3,"omitted":true}}),
            OutputFormat::Markdown,
            "code_map_explain",
            None,
            OperationKind::Navigation,
        );
        if !stub.contains("## Inbound relationships\n") {
            failures.push("compact stub lost heading".into());
        }
        let unres = render_presented(
            &json!({"nodes":{"total":2,"items":[{"name":"missing","path":"src/missing.rs","line":4},{"name":"unsafe","path":"../outside.rs","line":5}]}}),
            OutputFormat::Markdown,
            "code_map_search",
            Some(DiscoveryKind::Search),
        );
        if unres.contains("(link unavailable)")
            || !unres.contains("src/missing.rs")
            || !unres.contains("../outside.rs")
        {
            failures.push("search unresolved link check failed".into());
        }
        let unres_ins = render_presented(
            &json!({"node":{"qualname":"selected","path":"src/missing.rs","line":7},"governing_invariants":{"direct":[],"omitted_sources":[{"id":"INV-MISSING","source":{"path":"src/missing.rs","line":7}},{"id":"INV-UNSAFE","source":{"path":"../outside.rs"}}]}}),
            OutputFormat::Markdown,
            "code_map_inspect",
            Some(DiscoveryKind::Inspect),
        );
        if unres_ins.contains("link unavailable")
            || !unres_ins.contains("src/missing.rs")
            || !unres_ins.contains("../outside.rs")
        {
            failures.push("inspect unresolved link check failed".into());
        }
        let existing_uri = "file:///tmp/workspace/src/space%20dir/%23unicode-%CE%BB.rs#L9";
        let rendered_uri = render_presented(
            &json!({"nodes":{"total":1,"items":[{"name":"symbol","path":"src/space dir/#unicode-λ.rs","line":9,"uri":existing_uri}]}}),
            OutputFormat::Markdown,
            "code_map_search",
            Some(DiscoveryKind::Search),
        );
        if !rendered_uri.contains(existing_uri)
            || rendered_uri.contains("%2520")
            || rendered_uri.contains("%2523")
        {
            failures.push("pre-attached URI failed".into());
        }
        assert!(
            failures.is_empty(),
            "formatter contract failures: {failures:#?}"
        );
    }

    #[test]
    fn opaque_operations_are_never_compacted_by_field_name() {
        let value = json!({"generation":"opaque","metadata":{"generation":"user"},"items":[],"complete":false});
        for op in [
            OperationKind::Sql,
            OperationKind::Task,
            OperationKind::Guide,
            OperationKind::General,
        ] {
            let mut actual = value.clone();
            compact(&mut actual, op, None);
            assert_eq!(actual, value);
        }
    }

    #[test]
    fn navigation_compaction_keeps_active_tokens_and_false_or_zero() {
        let mut value = json!({
            "count":0,"complete":false,
            "nodes":{"total":1,"offset":0,"limit":1,"has_more":false,"next_offset":null,"generation":"inactive-token","items":[{"name":"x"}]},
            "outgoing":{"total":2,"offset":0,"limit":1,"has_more":true,"next_offset":1,"generation":"active-token","items":[{"name":"y"}]},
            "freshness":{"status":"source_inventory_matched","output_root":"display-root","corpus_hash":"display-hash","refresh":"none"}
        });
        compact(&mut value, OperationKind::Navigation, None);
        assert!(value.get("generation").is_none());
        assert!(value["nodes"].get("generation").is_none());
        assert_eq!(value["outgoing"]["generation"], "active-token");
        assert_eq!(value["count"], 0);
        assert_eq!(value["complete"], false);
        assert!(value["freshness"].get("output_root").is_none());
    }
}
