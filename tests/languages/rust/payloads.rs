use crate::languages::harness::PersistedSnapshot;
use crate::languages::PersistedWorkspaceHarness;

fn member_rules(returns: Option<&str>) -> String {
    let mut manifest = String::from("enabled = true\nreplace = true\n");
    for package in ["serde_json", "serde_yaml"] {
        manifest.push_str(&format!("[[class_hierarchy_receivers]]\nbases = ['{package}']\nprovider = '{package}'\nmembers = ['from_str', 'from_slice', 'from_reader', 'from_value']\n"));
        if let Some(base) = returns {
            manifest.push_str("[class_hierarchy_receivers.returns]\n");
            for member in ["from_str", "from_slice", "from_reader", "from_value"] {
                manifest.push_str(&format!(
                    "{member} = {{ kind = 'receiver', base = '{base}' }}\n"
                ));
            }
        }
    }
    manifest
}

fn policy_workspace(manifest: Option<&str>) -> (PersistedWorkspaceHarness, Vec<String>) {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write("Cargo.toml", "[package]\nname='batch11-probe'\nversion='0.1.0'\n[dependencies]\nserde_json='1'\nserde_yaml='0.9'\nserde={version='1',features=['derive']}\n");
    if let Some(manifest) = manifest {
        workspace.write(".acdd/frameworks/rust-cargo.toml", manifest);
    }
    let mut source = String::from("#[derive(serde::Deserialize)] struct LeafBatch11 {} impl LeafBatch11 { fn finish(&self) {} }\n");
    let mut functions = Vec::new();
    for (package, alias, methods) in [
        (
            "serde_json",
            "json",
            &["from_str", "from_slice", "from_reader", "from_value"][..],
        ),
        (
            "serde_yaml",
            "yaml",
            &["from_str", "from_slice", "from_reader"][..],
        ),
    ] {
        source.push_str(&format!("use {package} as {alias};\n"));
        for method in methods {
            source.push_str(&format!("use {package}::{method} as {alias}_{method};\n"));
            let reader = if *method == "from_reader" { "_, " } else { "" };
            let input = match *method {
                "from_slice" => "b\"{}\"",
                "from_reader" => "\"{}\".as_bytes()",
                "from_value" => "serde_json::Value::Null",
                _ => "\"{}\"",
            };
            for (spelling, callee) in [
                ("direct", format!("{package}::{method}")),
                ("namespace", format!("{alias}::{method}")),
                ("function", format!("{alias}_{method}")),
            ] {
                let function = format!("probe_{alias}_{method}_{spelling}");
                source.push_str(&format!("fn {function}() {{ {callee}::<{reader}LeafBatch11>({input}).unwrap().finish(); }}\n"));
                source.push_str(&format!("fn {function}_assigned() {{ let parsed = {callee}::<{reader}LeafBatch11>({input}).expect(\"valid\"); parsed.finish(); }}\n"));
                source.push_str(&format!("fn {function}_annotated() {{ let parsed: Result<LeafBatch11, {package}::Error> = {callee}({input}); parsed.unwrap().finish(); }}\n"));
                functions.push(function);
            }
        }
    }
    workspace.write("src/lib.rs", &source);
    workspace.build();
    (workspace, functions)
}

#[test]
fn effective_cargo_policy_controls_inferred_deserialization_payloads() {
    for (policy, manifest, inferred) in [
        ("enabled", None, true),
        ("disabled", Some("enabled = false\n".to_owned()), false),
        (
            "replace-empty",
            Some("enabled = true\nreplace = true\n".to_owned()),
            false,
        ),
        ("replace-no-returns", Some(member_rules(None)), false),
        ("replace-result", Some(member_rules(Some("Result"))), true),
        (
            "replace-non-result",
            Some(member_rules(Some("String"))),
            false,
        ),
    ] {
        let (workspace, functions) = policy_workspace(manifest.as_deref());
        let db = workspace.open();
        for function in functions {
            for (suffix, expected) in [
                ("", inferred),
                ("_assigned", inferred),
                ("_annotated", true),
            ] {
                let function = format!("{function}{suffix}");
                let edges: i64 = db.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.qualname LIKE '%LeafBatch11.finish' AND e.kind='calls'", [&function], |row| row.get(0)).unwrap();
                assert_eq!(
                    edges,
                    i64::from(expected),
                    "{policy}: {function}: effective signature authority"
                );
                if !expected {
                    let resolved: i64 = db.query_row("SELECT count(*) FROM resolution_coverage c JOIN coverage_expressions x USING(expression_id) JOIN nodes n ON n.path_id=c.path_id AND c.line BETWEEN n.line AND n.end_line WHERE n.name=?1 AND x.expression LIKE '%finish' AND c.status!='unresolved'", [&function], |row| row.get(0)).unwrap();
                    assert_eq!(
                        resolved, 0,
                        "{policy}: {function}: unknown inferred payload must fail closed"
                    );
                }
            }
        }
    }
}

fn generic_workspace() -> (PersistedWorkspaceHarness, Vec<String>) {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write("Cargo.toml", "[package]\nname='batch10-probe'\nversion='0.1.0'\n[dependencies]\nserde_json='1'\nserde_yaml='0.9'\nserde={version='1',features=['derive']}\n");
    let mut source = String::from(
        r#"
#[derive(serde::Deserialize)] struct LeafBatch10 {}
impl LeafBatch10 { fn finish(&self) {} }
struct HubBatch10 {}
impl HubBatch10 { fn leaf(&self) -> LeafBatch10 { LeafBatch10 {} } }
struct ChangingBatch10 {}
impl ChangingBatch10 {
    fn unwrap(self) -> LeafBatch10 { LeafBatch10 {} }
    fn expect(self, _: &str) -> LeafBatch10 { LeafBatch10 {} }
}
fn missing_unwrap(h: HubBatch10) { h.unwrap().leaf(); }
fn missing_expect(h: HubBatch10) { h.expect("missing").leaf(); }
fn missing_try(h: HubBatch10) -> Result<(), ()> { h?.leaf(); Ok(()) }
fn assigned_missing(h: HubBatch10) { let changed = h.unwrap(); changed.leaf(); }
fn declared_unwrap(h: ChangingBatch10) { h.unwrap().finish(); }
fn declared_expect(h: ChangingBatch10) { h.expect("valid").finish(); }
fn known_option(opt: Option<LeafBatch10>) { opt.unwrap().finish(); }
fn known_result(res: Result<LeafBatch10, ()>) { res.expect("valid").finish(); }
fn missing_external(path: std::path::PathBuf) { path.unwrap().exists(); path.expect("missing").exists(); }
use serde_json as json_alias;
use serde_yaml::from_str as read_yaml;
fn json_alias_payload() { json_alias::from_str::<LeafBatch10>("{}").unwrap().finish(); }
fn yaml_import_payload() { read_yaml::<LeafBatch10>("{}").unwrap().finish(); }
fn annotated_json_payload() {
    let result: Result<LeafBatch10, serde_json::Error> = serde_json::from_str("{}");
    result.unwrap().finish();
}
"#,
    );
    let mut cases = Vec::new();
    for (package, methods) in [
        (
            "serde_json",
            &["from_str", "from_slice", "from_reader", "from_value"][..],
        ),
        ("serde_yaml", &["from_str", "from_slice", "from_reader"][..]),
    ] {
        for method in methods {
            let function = format!("{package}_{method}");
            let reader_type = if *method == "from_reader" { "_, " } else { "" };
            let input = match *method {
                "from_slice" => "b\"{}\"",
                "from_reader" => "\"{}\".as_bytes()",
                "from_value" => "serde_json::Value::Null",
                _ => "\"{}\"",
            };
            source.push_str(&format!("fn {function}() {{ let custom = {package}::{method}::<{reader_type}LeafBatch10>({input}).unwrap(); custom.finish(); let text = {package}::{method}::<{reader_type}String>({input}).unwrap(); text.len(); let value = {package}::{method}::<{reader_type}{package}::Value>({input}).unwrap(); value.is_null(); }}\n"));
            source.push_str(&format!("fn {function}_unknown<T: serde::de::DeserializeOwned>() {{ let unknown = {package}::{method}::<{reader_type}T>({input}).unwrap(); unknown.finish(); let untyped = {package}::{method}({input}).unwrap(); untyped.finish(); }}\n"));
            source.push_str(&format!("fn {function}_direct() {{ {package}::{method}::<{reader_type}LeafBatch10>({input}).unwrap().finish(); }}\n"));
            cases.push(function);
        }
    }
    workspace.write("src/lib.rs", &source);
    workspace.build();
    (workspace, cases)
}

#[test]
fn persisted_wrapper_payload_identity() {
    let (workspace, functions) = generic_workspace();
    let snapshot = PersistedSnapshot::from_connection(&workspace.open());
    drop(workspace);
    let db = snapshot.open();
    // Each row names the original observable contract and carries its query inputs.
    enum Expected<'a> {
        Calls {
            target: &'a str,
            exact_leaf: bool,
            count: i64,
        },
        Coverage {
            expression: &'a str,
            status: &'a str,
            origin: Option<&'a str>,
        },
    }
    let mut cases = Vec::new();
    for function in functions {
        for name in [function.clone(), format!("{function}_direct")] {
            cases.push((
                "explicit_payload_generics_keep_local_and_external_type_identity",
                name,
                Expected::Calls {
                    target: "finish",
                    exact_leaf: true,
                    count: 1,
                },
            ));
        }
        for (expression, origin) in [
            ("text.len", "Rust standard library"),
            ("value.is_null", "Value"),
        ] {
            cases.push((
                "explicit_payload_generics_keep_local_and_external_type_identity",
                function.clone(),
                Expected::Coverage {
                    expression,
                    status: "external",
                    origin: Some(origin),
                },
            ));
        }
        let unknown = format!("{function}_unknown");
        cases.push((
            "explicit_payload_generics_keep_local_and_external_type_identity",
            unknown.clone(),
            Expected::Calls {
                target: "finish",
                exact_leaf: false,
                count: 0,
            },
        ));
        for expression in ["unknown.finish", "untyped.finish"] {
            cases.push((
                "explicit_payload_generics_keep_local_and_external_type_identity",
                unknown.clone(),
                Expected::Coverage {
                    expression,
                    status: "unresolved",
                    origin: None,
                },
            ));
        }
    }
    for (function, expression) in [
        ("missing_unwrap", "h.unwrap().leaf"),
        ("missing_expect", "h.expect(\"missing\").leaf"),
        ("missing_try", "h?.leaf"),
        ("assigned_missing", "changed.leaf"),
    ] {
        cases.push((
            "unproved_wrapper_projections_cannot_revive_the_original_receiver",
            function.to_owned(),
            Expected::Calls {
                target: "leaf",
                exact_leaf: false,
                count: 0,
            },
        ));
        cases.push((
            "unproved_wrapper_projections_cannot_revive_the_original_receiver",
            function.to_owned(),
            Expected::Coverage {
                expression,
                status: "unresolved",
                origin: None,
            },
        ));
    }
    for function in [
        "declared_unwrap",
        "declared_expect",
        "known_option",
        "known_result",
        "json_alias_payload",
        "yaml_import_payload",
        "annotated_json_payload",
    ] {
        cases.push((
            "declared_and_known_wrapper_payloads_retain_the_exact_leaf_target",
            function.to_owned(),
            Expected::Calls {
                target: "finish",
                exact_leaf: false,
                count: 1,
            },
        ));
    }
    for expression in ["path.unwrap().exists", "path.expect(\"missing\").exists"] {
        cases.push((
            "external_non_wrappers_do_not_gain_wrapper_projections",
            "missing_external".to_owned(),
            Expected::Coverage {
                expression,
                status: "unresolved",
                origin: None,
            },
        ));
    }
    for (case_id, function, expected) in cases {
        match expected {
            Expected::Calls {
                target,
                exact_leaf,
                count,
            } => {
                let observed: i64 = db.query_row("SELECT count(*) FROM edges e JOIN nodes s ON s.node_hash=e.src_hash JOIN nodes d ON d.node_hash=e.dst_hash WHERE s.name=?1 AND d.name=?2 AND e.kind='calls' AND (?3=0 OR d.qualname LIKE '%LeafBatch10.finish')", rusqlite::params![function, target, exact_leaf], |row| row.get(0)).unwrap();
                assert_eq!(observed, count, "{case_id}: {function}: {target}");
            }
            Expected::Coverage {
                expression,
                status,
                origin,
            } => {
                let (observed, evidence): (String, String) = db.query_row("SELECT c.status, v.evidence FROM resolution_coverage c JOIN coverage_expressions x USING(expression_id) JOIN coverage_evidence v USING(evidence_id) JOIN nodes n ON n.path_id=c.path_id AND c.line BETWEEN n.line AND n.end_line WHERE n.name=?1 AND x.expression=?2", [function.as_str(), expression], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                assert_eq!(
                    observed, status,
                    "{case_id}: {function}: {expression}: {evidence}"
                );
                if let Some(origin) = origin {
                    assert!(
                        evidence.contains(origin),
                        "{case_id}: {function}: {expression}: expected {origin}, got {evidence}"
                    );
                }
            }
        }
    }
}
