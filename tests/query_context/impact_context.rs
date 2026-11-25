use crate::common::Workspace;
use anyhow::Result;
use acdd_core::{
    format::stable_hash64,
    response::{Detail, QueryOptions, ResponsePolicy},
};
use acdd_index::db::{schema::SCHEMA_DDL, traversal};
use rusqlite::{params, Connection};
fn database(workspace: &Workspace) -> Result<Connection> {
    let conn = Connection::open(workspace.path("graph.sqlite"))?;
    conn.execute_batch(SCHEMA_DDL)?;
    conn.execute(
        "INSERT INTO metadata(key,value) VALUES('output_root','test-generation')",
        [],
    )?;
    conn.execute_batch("INSERT INTO path_dictionary(path_id,path) VALUES(1,'fixture.py'),(2,'module.py'); INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'fixture'),(2,'exact'),(3,'self');")?;
    for id in ["a", "b", "c", "d", "decorator"] {
        conn.execute("INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) VALUES(?1,'function',?1,?1,1,1,1,0,'python',0,'{}',?2,1)", params![id, stable_hash64(id)])?;
    }
    for (from, to, kind) in [
        ("a", "b", "calls"),
        ("a", "c", "calls"),
        ("b", "d", "calls"),
        ("c", "d", "calls"),
        ("d", "a", "calls"),
        ("decorator", "b", "decorates"),
    ] {
        conn.execute("INSERT INTO edges(src_hash,dst_hash,kind,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES(?1,?2,?3,1,1,1,2,1)", params![stable_hash64(from),stable_hash64(to),kind])?;
    }
    Ok(conn)
}

fn options(limit: usize, offset: usize, detail: Detail) -> Result<QueryOptions> {
    QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(limit),
        offset,
        Some(detail),
        (offset > 0).then(|| "test-generation".into()),
    )
}

#[test]
fn impact_pages_explain_shortest_links_without_repeating_graph_evidence() -> Result<()> {
    let workspace = Workspace::new();
    let conn = database(&workspace)?;
    let first = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(2, 0, Detail::Compact)?,
        },
    )?;
    assert_eq!(first["nodes"]["total"], 5);
    assert_eq!(first["nodes"]["items"][0]["id"], "a");
    assert_eq!(first["nodes"]["items"][0]["is_seed"], true);
    assert!(first["nodes"]["items"][0].get("edge").is_none());
    assert!(first["nodes"]["items"][0].get("via").is_none());
    assert_eq!(first["nodes"]["items"][1]["id"], "b");
    assert_eq!(first["nodes"]["items"][1]["via"]["id"], "a");
    assert_eq!(first["nodes"]["items"][1]["via"]["kind"], "calls");
    assert!(first["nodes"]["items"][1].get("evidence").is_none());
    let markdown = contextunity_acdd::format::render_for_operation(
        &first,
        contextunity_acdd::format::OutputFormat::Markdown,
        "code_map_impact",
        None,
        contextunity_acdd::format::OperationKind::Navigation,
    );
    assert!(markdown.contains("## Reached nodes"), "{markdown}");
    assert!(
        markdown.contains("Via ")
            && markdown.contains("using")
            && markdown.contains("calls")
            && markdown.contains("this depth-grouped path has no call-site coordinate"),
        "depth-grouped paths retain their relation without invented edge coordinates: {markdown}"
    );

    let second = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(3, 2, Detail::Compact)?,
        },
    )?;
    assert_eq!(second["nodes"]["total"], 5);
    let items = second["nodes"]["items"].as_array().expect("page items");
    assert_eq!(
        items
            .iter()
            .map(|item| item["id"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        vec!["c", "d", "decorator"]
    );
    assert_eq!(items[1]["distance"], 2);
    assert_eq!(items[1]["via"]["id"], "b");
    assert_eq!(items[2]["via"]["id"], "b");
    assert_eq!(items[2]["via"]["kind"], "decorates");
    assert_eq!(second["nodes"]["has_more"], false);

    let full = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(5, 0, Detail::Full)?,
        },
    )?;
    assert_eq!(full["nodes"]["items"][4]["via"], items[2]["via"]);
    assert_eq!(full["nodes"]["items"][4]["is_seed"], false);
    let beyond = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(2, 100, Detail::Compact)?,
        },
    )?;
    assert_eq!(beyond["nodes"]["total"], 5);
    assert!(beyond["nodes"]["items"]
        .as_array()
        .expect("page items")
        .is_empty());

    let zero = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 0,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(5, 0, Detail::Compact)?,
        },
    )?;
    assert_eq!(zero["nodes"]["total"], 1);
    assert_eq!(zero["nodes"]["items"][0]["is_seed"], true);
    let inbound = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 1,
            direction: traversal::ImpactDirection::Inbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(5, 0, Detail::Compact)?,
        },
    )?;
    assert_eq!(inbound["nodes"]["items"][1]["id"], "d");
    assert_eq!(inbound["nodes"]["items"][1]["is_seed"], false);
    assert!(inbound["nodes"]["items"][1].get("via").is_none());
    assert_eq!(inbound["nodes"]["items"][1]["edge"]["kind"], "calls");
    assert_eq!(inbound["nodes"]["items"][1]["edge"]["direction"], "inbound");
    assert_eq!(inbound["nodes"]["items"][1]["edge"]["path"], "fixture.py");
    assert_eq!(inbound["nodes"]["items"][1]["edge"]["line"], 1);
    let shallow_reverse = traversal::impact::traverse_with_path_options(
        &conn,
        "b",
        None,
        &traversal::TraversalOptions {
            depth: 1,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(5, 0, Detail::Compact)?,
        },
    )?;
    let decorator = shallow_reverse["nodes"]["items"]
        .as_array()
        .expect("page items")
        .iter()
        .find(|item| item["id"] == "decorator")
        .expect("decorator");
    assert_eq!(decorator["distance"], 1);
    assert!(decorator.get("via").is_none());
    assert_eq!(decorator["edge"]["kind"], "decorates");
    assert_eq!(decorator["edge"]["direction"], "inbound");
    assert_eq!(decorator["edge"]["path"], "fixture.py");
    assert_eq!(decorator["edge"]["line"], 1);
    conn.execute("INSERT INTO edges(src_hash,dst_hash,kind,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES(?1,?1,'calls',1,1,3,2,1)",[stable_hash64("a")])?;
    let shallow_self = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 1,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(5, 0, Detail::Compact)?,
        },
    )?;
    assert_eq!(shallow_self["nodes"]["total"], 3);
    assert_eq!(shallow_self["nodes"]["items"][0]["distance"], 0);
    Ok(())
}

#[test]
fn structural_dependency_uses_the_same_edge_kind_for_reachability_and_explanation() -> Result<()> {
    let workspace = Workspace::new();
    let conn = database(&workspace)?;
    conn.execute("INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) VALUES('module','module','module','module',2,1,1,0,'python',0,'{}',?1,2)", [stable_hash64("module")])?;
    conn.execute("INSERT INTO edges(src_hash,dst_hash,kind,path_id,line,evidence_id,confidence_id,occurrence_count) VALUES(?1,?2,'imports',1,1,1,2,1)",params![stable_hash64("a"),stable_hash64("module")])?;
    let page = traversal::impact::traverse_with_path_options(
        &conn,
        "a",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(10, 0, Detail::Compact)?,
        },
    )?;
    let imported = page["nodes"]["items"]
        .as_array()
        .expect("page items")
        .iter()
        .find(|item| item["id"] == "module")
        .expect("imported module");
    assert_eq!(imported["distance"], 1);
    assert_eq!(imported["via"]["id"], "a");
    assert_eq!(imported["via"]["kind"], "imports");
    Ok(())
}

#[test]
fn removal_assessment_names_each_indexed_blocker_without_changing_verdict() -> Result<()> {
    let workspace = Workspace::new();
    let conn = database(&workspace)?;
    conn.execute_batch("INSERT INTO path_dictionary(path_id,path) VALUES(3,'orphan.py'),(4,'other.py'),(5,'caller.py'); INSERT INTO coverage_expressions(expression_id,expression) VALUES(1,'dynamic()'),(2,'orphan');")?;
    conn.execute("INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id) VALUES('orphan','function','orphan','orphan',3,1,1,0,'python',0,'{}',?1,3)", [stable_hash64("orphan")])?;
    let clean = traversal::removal_paged_with_path(&conn, "orphan", None, &options(10, 0, Detail::Compact)?)?;
    assert_eq!(clean["assessment"]["verdict"], "no_indexed_blockers");
    assert_eq!(clean["safe_to_remove"], true);
    let isolated = traversal::impact::traverse_with_path_options(
        &conn,
        "orphan",
        None,
        &traversal::TraversalOptions {
            depth: 16,
            direction: traversal::ImpactDirection::Outbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(10, 0, Detail::Compact)?,
        },
    )?;
    assert_eq!(isolated["nodes"]["total"], 1);
    assert_eq!(isolated["nodes"]["items"][0]["distance"], 0);
    assert_eq!(isolated["nodes"]["items"][0]["is_seed"], true);
    conn.execute("INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id) VALUES(4,1,1,'unresolved',1)",[])?;
    conn.execute(
        "INSERT INTO errors(path,line,message) VALUES('other.py',1,'syntax error')",
        [],
    )?;
    let unrelated = traversal::removal_paged_with_path(&conn, "orphan", None, &options(10, 0, Detail::Compact)?)?;
    assert_eq!(unrelated["safe_to_remove"], true);
    assert_eq!(unrelated["unresolved_references"], 0);
    assert_eq!(unrelated["parse_errors"], 0);
    for duplicate in [
        "target_safe_to_remove",
        "target_unresolved_references",
        "target_parse_errors",
    ] {
        assert!(unrelated.get(duplicate).is_none());
    }
    let result = traversal::removal_paged_with_path(&conn, "b", None, &options(10, 0, Detail::Compact)?)?;
    assert_eq!(result["assessment"]["verdict"], "blocked");
    assert_eq!(
        result["assessment"]["blocking_reasons"]
            .as_array()
            .expect("reasons")
            .len(),
        1
    );
    assert_eq!(
        result["assessment"]["blocking_reasons"][0]["kind"],
        "incoming_dependencies"
    );
    assert_eq!(
        result["assessment"]["blocking_reasons"][0]["scope"],
        "selection"
    );
    assert_eq!(result["assessment"]["blocking_reasons"][0]["count"], 1);
    assert_eq!(result["unresolved_references"], 0);
    assert_eq!(result["parse_errors"], 0);
    assert_eq!(result["safe_to_remove"], false);
    assert_eq!(result["incoming_dependencies"]["total"], 1);
    assert!(result["proof_scope"]
        .as_str()
        .unwrap_or("")
        .contains("indexed static references only"));

    // Target-scoped unresolved reference blocks removal
    conn.execute(
        "INSERT INTO resolution_coverage(path_id,line,expression_id,status,evidence_id) VALUES(5,1,2,'unresolved',1)",
        [],
    )?;
    let blocked_target =
        traversal::removal_paged_with_path(&conn, "orphan", None, &options(10, 0, Detail::Compact)?)?;
    assert_eq!(blocked_target["safe_to_remove"], false);
    assert_eq!(blocked_target["unresolved_references"], 1);
    assert_eq!(blocked_target["assessment"]["verdict"], "blocked");
    assert_eq!(
        blocked_target["assessment"]["blocking_reasons"][0]["kind"],
        "unresolved_references"
    );
    Ok(())
}

#[cfg(feature = "lang-python")]
#[test]
fn candidate_only_callers_appear_in_impact_and_block_removal() -> Result<()> {
    let workspace = Workspace::new();
    workspace.write(
        "branch.py",
        "class Alpha:\n    def execute(self): pass\nclass Beta:\n    def execute(self): pass\ndef run(condition):\n    if condition:\n        receiver = Alpha()\n    else:\n        receiver = Beta()\n    receiver.execute()\n",
    );
    workspace.build();
    let conn = workspace.open();
    let target_id: String = conn.query_row(
        "SELECT id FROM nodes WHERE name='execute' AND kind='method' AND qualname LIKE '%Alpha.execute'",
        [],
        |row| row.get(0),
    )?;
    let caller_id: String = conn.query_row(
        "SELECT id FROM nodes WHERE name='run' AND kind='function'",
        [],
        |row| row.get(0),
    )?;

    let impact = traversal::impact::traverse_with_path_options(
        &conn,
        &target_id,
        None,
        &traversal::TraversalOptions {
            depth: 1,
            direction: traversal::ImpactDirection::Inbound,
            mode: traversal::TraversalMode::Calls,
            edge_types: None,
            page: &options(10, 0, Detail::Full)?,
        },
    )?;
    assert!(
        impact["nodes"]["items"]
            .as_array()
            .expect("impact page")
            .iter()
            .any(|node| node["id"] == caller_id),
        "candidate caller appears in inbound impact"
    );
    let exact_call_count: usize = conn.query_row(
        "SELECT count(*) FROM edges WHERE src_hash=?1 AND dst_hash=?2 AND kind='calls'",
        [stable_hash64(&caller_id), stable_hash64(&target_id)],
        |row| row.get(0),
    )?;
    assert_eq!(
        exact_call_count, 0,
        "fixture must have only a candidate caller"
    );

    let removal = traversal::removal_paged_with_path(&conn, &target_id, None, &options(10, 0, Detail::Full)?)?;
    assert_eq!(removal["safe_to_remove"], false);
    assert_eq!(removal["assessment"]["verdict"], "blocked");
    let incoming = removal["incoming_dependencies"]["items"]
        .as_array()
        .expect("incoming dependency page");
    assert!(incoming.iter().any(|edge| {
        edge["src_public_id"] == caller_id
            && edge["kind"] == "possible_calls"
            && edge["confidence"] == "possible"
    }));
    Ok(())
}

#[test]
fn dead_tails_classifies_cross_crate_liveness_chains_phantom_tested_and_dangling_calls() -> Result<()>
{
    let workspace = Workspace::new();
    let conn = Connection::open(workspace.path("dead_tails.sqlite"))?;
    conn.execute_batch(SCHEMA_DDL)?;
    conn.execute(
        "INSERT INTO metadata(key,value) VALUES('output_root','test-generation')",
        [],
    )?;
    conn.execute_batch(
        "INSERT INTO path_dictionary(path_id,path) VALUES
            (1,'crates/a/src/main.rs'),
            (2,'crates/b/src/lib.rs'),
            (3,'crates/b/tests/b_tests.rs'),
            (4,'src/mcp/tools.rs');
         INSERT INTO coverage_evidence(evidence_id,evidence) VALUES(1,'fixture'),(2,'exact');",
    )?;

    let insert_node = |id: &str,
                       kind: &str,
                       name: &str,
                       path_id: i64,
                       line: i64,
                       end_line: i64,
                       is_test: i64,
                       details: &str|
     -> Result<()> {
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id)
             VALUES(?1,?2,?3,?1,?4,?5,?6,?7,'rust',0,?8,?9,?4)",
            params![
                id,
                kind,
                name,
                path_id,
                line,
                end_line,
                is_test,
                details,
                stable_hash64(id)
            ],
        )?;
        Ok(())
    };

    // Roots: main in main.rs, explicit entrypoint, trait impl container + implements, struct + serde ref, #[tool]
    insert_node("a_main", "function", "main", 1, 1, 5, 0, "{}")?;
    insert_node(
        "a_entry",
        "function",
        "entry_hook",
        1,
        7,
        10,
        0,
        r#"{"entrypoint":"cli"}"#,
    )?;
    insert_node("b_cross_live", "function", "cross_live", 2, 10, 15, 0, "{}")?;
    insert_node(
        "b_cross_live_leaf",
        "function",
        "cross_live_leaf",
        2,
        17,
        20,
        0,
        "{}",
    )?;
    insert_node(
        "b_dead_chain_a",
        "function",
        "dead_chain_a",
        2,
        22,
        35,
        0,
        "{}",
    )?;
    insert_node(
        "b_dead_chain_b",
        "function",
        "dead_chain_b",
        2,
        37,
        42,
        0,
        "{}",
    )?;
    insert_node("b_phantom_a", "function", "phantom_a", 2, 44, 60, 0, "{}")?;
    insert_node("b_phantom_b", "function", "phantom_b", 2, 62, 68, 0, "{}")?;
    insert_node("b_serde_hook", "function", "serde_hook", 2, 70, 72, 0, "{}")?;
    insert_node("b_cfg_struct", "struct", "Cfg", 2, 74, 78, 0, "{}")?;
    insert_node(
        "b_trait_impl",
        "impl",
        "Cfg",
        2,
        80,
        86,
        0,
        r#"{"impl_trait":"MyTrait"}"#,
    )?;
    insert_node(
        "b_trait_method",
        "method",
        "impl_trait_method",
        2,
        81,
        85,
        0,
        "{}",
    )?;
    insert_node(
        "b_implemented_fn",
        "method",
        "implemented_fn",
        2,
        88,
        91,
        0,
        "{}",
    )?;
    insert_node("b_trait_decl", "trait", "MyTrait", 2, 93, 97, 0, "{}")?;
    insert_node("b_dead_cycle_a", "function", "dead_cycle_a", 2, 98, 104, 0, "{}")?;
    insert_node("b_dead_cycle_b", "function", "dead_cycle_b", 2, 106, 110, 0, "{}")?;
    insert_node("b_status_field", "field", "status", 2, 75, 75, 0, "{}")?;
    conn.execute(
        "INSERT INTO nodes(id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id)
         VALUES('b_gen_macro','function','gen_macro_fn','b_gen_macro',2,112,115,0,'rust',1,'{}',?1,2)",
        params![stable_hash64("b_gen_macro")],
    )?;
    insert_node("b_gen_callee", "function", "gen_callee", 2, 117, 120, 0, "{}")?;
    insert_node("b_local_impl", "impl", "LocalHelper", 2, 12, 14, 0, "{}")?;
    insert_node("b_nested_reap", "method", "nested_reap", 2, 13, 14, 0, "{}")?;
    insert_node("b_decode_row", "function", "decode_row", 2, 122, 126, 0, "{}")?;
    insert_node("b_apply_v2", "function", "apply_v2", 2, 128, 132, 0, "{}")?;
    insert_node(
        "b_test_case",
        "function",
        "test_phantom_entry",
        3,
        5,
        12,
        1,
        "{}",
    )?;
    insert_node(
        "mcp_tool",
        "method",
        "code_map_query",
        4,
        10,
        20,
        0,
        r##"{"decorators":["#[tool(description=\"x\")]"]}"##,
    )?;

    for (src, dst, kind, path_id) in [
        ("a_main", "b_cross_live", "calls", 1),
        ("b_cross_live", "b_cross_live_leaf", "calls", 2),
        ("b_cross_live", "b_local_impl", "contains", 2),
        ("b_local_impl", "b_nested_reap", "contains", 2),
        ("b_dead_chain_a", "b_dead_chain_b", "calls", 2),
        ("b_dead_chain_b", "b_decode_row", "calls", 2),
        ("b_dead_cycle_a", "b_dead_cycle_b", "calls", 2),
        ("b_dead_cycle_b", "b_dead_cycle_a", "calls", 2),
        ("b_gen_macro", "b_gen_callee", "calls", 2),
        ("b_test_case", "b_phantom_a", "calls", 3),
        ("b_phantom_a", "b_phantom_b", "calls", 2),
        ("b_cfg_struct", "b_serde_hook", "references", 2),
        ("b_trait_impl", "b_trait_method", "contains", 2),
        ("b_implemented_fn", "b_trait_decl", "implements", 2),
    ] {
        conn.execute(
            "INSERT INTO edges(src_hash,dst_hash,kind,path_id,line,evidence_id,confidence_id,occurrence_count)
             VALUES(?1,?2,?3,?4,1,1,2,1)",
            params![stable_hash64(src), stable_hash64(dst), kind, path_id],
        )?;
    }

    for (owner_id, ordinal, symbol) in [
        (2, 1, "crate::missing::deleted_fn"),
        (2, 2, "super::gone_helper"),
        (2, 3, "Self::new"),
        (2, 4, "crate::a::b().chain"),
        (2, 5, "crate::b::Cfg::Variant"),
        (2, 6, "crate::b::cross_live"),
        (2, 7, "crate::missing::status"),
        (2, 8, "conn.query_row(&sql, params![], decode_row)"),
        (2, 9, "MigrationStep { target_version: 2, apply: apply_v2 }"),
        (2, 10, "fn caller() { let dead_chain_b = 1; }"),
        (1, 1, "self::removed_local"),
    ] {
        conn.execute(
            "INSERT INTO dependencies(owner_id,ordinal,target_hash,kind,symbol,resolution)
             VALUES(?1,?2,NULL,'unresolved',?3,'unresolved')",
            params![owner_id, ordinal, symbol],
        )?;
    }

    let mut crate_b_facts = acdd_index::typed_facts::TypedFacts::default();
    for (line, expr) in [(11, "decode_row"), (12, "apply_v2")] {
        crate_b_facts.facts.references.push(acdd_index::models::Reference {
            source: "b_cross_live".to_owned(),
            line,
            column: 0,
            expression: expr.to_owned(),
            kind: "references".to_owned(),
            alias: None,
            module: None,
            dynamic: false,
            receiver_hint: None,
        });
    }
    let facts_blob = acdd_index::typed_facts::encode_facts(&crate_b_facts)?;
    conn.execute(
        "INSERT INTO local_facts(path,source_digest,language,is_test,generated,facts_blob)
         VALUES('crates/b/src/lib.rs','fixture-digest','rust',0,0,?1)",
        params![facts_blob],
    )?;

    let names = |page: &serde_json::Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let symbols = |page: &serde_json::Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["symbol"].as_str().unwrap().to_owned())
            .collect()
    };

    struct DeadTailsCase<'a> {
        selector: Option<&'a str>,
        path: Option<&'a str>,
        limit: usize,
        offset: usize,
        exp_phantom: &'a [&'a str],
        exp_dead: &'a [&'a str],
        exp_dangling: &'a [&'a str],
    }

    let cases = &[
        DeadTailsCase {
            selector: None,
            path: None,
            limit: 30,
            offset: 0,
            exp_phantom: &["phantom_a", "phantom_b"],
            exp_dead: &["dead_chain_a", "dead_cycle_a", "dead_chain_b", "dead_cycle_b"],
            exp_dangling: &[
                "self::removed_local",
                "crate::missing::deleted_fn",
                "crate::missing::status",
                "super::gone_helper",
            ],
        },
        DeadTailsCase {
            selector: Some("crates/b"),
            path: None,
            limit: 30,
            offset: 0,
            exp_phantom: &["phantom_a", "phantom_b"],
            exp_dead: &["dead_chain_a", "dead_cycle_a", "dead_chain_b", "dead_cycle_b"],
            exp_dangling: &[
                "crate::missing::deleted_fn",
                "crate::missing::status",
                "super::gone_helper",
            ],
        },
        DeadTailsCase {
            selector: None,
            path: Some("crates/b"),
            limit: 30,
            offset: 0,
            exp_phantom: &["phantom_a", "phantom_b"],
            exp_dead: &["dead_chain_a", "dead_cycle_a", "dead_chain_b", "dead_cycle_b"],
            exp_dangling: &[
                "crate::missing::deleted_fn",
                "crate::missing::status",
                "super::gone_helper",
            ],
        },
        DeadTailsCase {
            selector: Some("crates/b"),
            path: Some("crates/b"),
            limit: 2,
            offset: 2,
            exp_phantom: &[],
            exp_dead: &["dead_chain_b", "dead_cycle_b"],
            exp_dangling: &["super::gone_helper"],
        },
    ];

    for case in cases {
        let page = options(case.limit, case.offset, Detail::Compact)?;
        let res = traversal::query_with_path_options(
            &conn,
            "dead_tails",
            case.selector,
            case.path,
            &traversal::GraphQueryOptions {
                depth: 2,
                direction: traversal::ImpactDirection::Inbound,
                coverage: acdd_core::response::CoverageOptions::default(),
                page: &page,
            },
        )?;
        assert_eq!(res["operation"], "dead_tails");
        assert_eq!(names(&res["phantom_tested"]), case.exp_phantom);
        assert_eq!(names(&res["dead_tails"]), case.exp_dead);
        assert_eq!(symbols(&res["dangling_calls"]), case.exp_dangling);

        if case.offset == 0 {
            let p0 = &res["phantom_tested"]["items"][0];
            assert_eq!(p0["test_caller_count"], 1);
            assert_eq!(p0["test_callers"][0]["name"], "test_phantom_entry");
            assert_eq!(res["dead_tails"]["items"][0]["dead_callees_count"], 1);
            assert_eq!(res["dead_tails"]["items"][1]["dead_callees_count"], 1);
            assert_eq!(res["dead_tails"]["items"][2]["dead_callees_count"], 0);
            assert_eq!(res["dead_tails"]["items"][3]["dead_callees_count"], 1);
        }

        let mut compacted = res.clone();
        if let Some(items) = compacted["dead_tails"]["items"].as_array_mut() {
            for item in items {
                item["uri"] = serde_json::json!(format!(
                    "file:///repo/{}#L{}",
                    item["path"].as_str().unwrap(),
                    item["line"].as_i64().unwrap()
                ));
            }
        }
        contextunity_acdd::format::compact(
            &mut compacted,
            contextunity_acdd::format::OperationKind::Navigation,
            None,
        );
        let md = contextunity_acdd::format::render_for_operation(
            &compacted,
            contextunity_acdd::format::OutputFormat::Markdown,
            "code_map_query",
            None,
            contextunity_acdd::format::OperationKind::Navigation,
        );
        assert!(md.contains("## Dead tails & phantom-tested code"), "{md}");
        assert!(md.contains("`crates/b/src/lib.rs:"), "{md}");
        assert!(!md.contains("file:///repo/"), "dead_tails markdown must use repo-relative paths: {md}");
        assert!(md.contains("## Dangling internal calls"), "{md}");
        if case.selector == Some("crates/b") && case.offset == 0 {
            assert!(md.contains("crate::missing::deleted_fn"), "{md}");
        }
    }

    let conflict = traversal::query_with_path_options(
        &conn,
        "dead_tails",
        Some("crates/a"),
        Some("crates/b"),
        &traversal::GraphQueryOptions {
            depth: 1,
            direction: traversal::ImpactDirection::Inbound,
            coverage: Default::default(),
            page: &options(10, 0, Detail::Compact)?,
        },
    );
    assert!(conflict.is_err(), "conflicting target vs path must fail");

    Ok(())
}
