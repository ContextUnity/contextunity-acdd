use super::*;
use crate::languages::harness::PersistedSnapshot;
use crate::languages::PersistedWorkspaceHarness;

const MANIFEST: &str = ".acdd/frameworks/property-proof.toml";

struct FactoryWorkspace {
    workspace: PersistedWorkspaceHarness,
    manifest_source: String,
    disabled_source: String,
}

impl FactoryWorkspace {
    fn new() -> Self {
        let workspace = crate::languages::PersistedWorkspaceHarness::new();
        for (path, source) in [
            (
                "factory.ts",
                "function run() { const button = document.createElement('button'); button.addEventListener('click', () => {}); const alias = button; alias.addEventListener('click', () => {}); const root = document.querySelector('#root'); if (root) root.querySelector('.child'); const found = document.getElementById('root'); if (found) found.getAttribute('id'); button.unknownMethod(); }",
            ),
            (
                "shadow.ts",
                "function run(document) { const button = document.createElement('button'); button.addEventListener('click', () => {}); }",
            ),
            (
                "reassigned_document.ts",
                "function run() { document = {}; const button = document.createElement('button'); button.addEventListener('click', () => {}); }",
            ),
            (
                "reassigned_result.ts",
                "function run() { let button = document.createElement('button'); button = {}; button.addEventListener('click', () => {}); }",
            ),
            (
                "shadow.js",
                "function run(document) { document.addEventListener('click', event => event.preventDefault()); }",
            ),
            (
                "unknown_factory.ts",
                "function run() { const button = document.makeElement('button'); button.addEventListener('click', () => {}); }",
            ),
            (
                "untyped.ts",
                "function run() { const button = {}; button.addEventListener('click', () => {}); }",
            ),
            (
                "local.ts",
                "class Element { addEventListener() {} } function run() { const button = new Element(); button.addEventListener('click'); }",
            ),
            (
                "literal.ts",
                "function run() { const input = document.createElement('input'); input.value.trim(); const script = document.createElement('script'); script.src.trim(); document.createElement('input').value.trim(); document.createElement('script').src.trim(); input.src.trim(); script.value.trim(); input.invented();\ninput.value(); }",
            ),
            (
                "literal.js",
                "function run() { const input = document.createElement('input'); input.value.trim(); const script = document.createElement('script'); script.src.trim(); document.createElement('input').value.trim(); document.createElement('script').src.trim(); input.src.trim(); script.value.trim(); input.invented();\ninput.value(); }",
            ),
            (
                "literal_shadow.ts",
                "function run(document) { const input = document.createElement('input'); input.value.trim(); document.createElement('script').src.trim(); }",
            ),
            (
                "literal_write.ts",
                "function run() { document = {}; const input = document.createElement('input'); input.value.trim(); }",
            ),
            (
                "literal_unknown.ts",
                "function run(tag) { const dynamic = document.createElement(tag); dynamic.addEventListener('click', () => {}); dynamic.value.trim(); const missing = document.createElement(); missing.addEventListener('click', () => {}); const spread = document.createElement(...['input']); spread.addEventListener('click', () => {}); spread.value.trim(); const excess = document.createElement('input', 1, 2, 3, 4); excess.addEventListener('click', () => {}); excess.value.trim(); const unknown = document.createElement('unmapped'); unknown.addEventListener('click', () => {}); unknown.value.trim(); }",
            ),
        ] {
            workspace.write(path, source);
        }
        for extension in ["ts", "js"] {
            workspace.write(
                format!("property.{extension}"),
                r#"function read() {
const input = document.createElement('input');
const script = document.createElement('script');
const a = input.value;
const b = script.src;
const c = document.createElement('input').value;
const d = document.createElement('script').src;
a.trim(); b.trim(); c.trim(); d.trim();
const wrong = document.createElement('input').src;
const other = document.createElement('script').value;
const dynamic = document.createElement(tag).value;
const spread = document.createElement(...['input']).value;
const fifth = document.createElement('input', 1, 2, 3, 4).value;
const unknown = document.createElement('unmapped').value;
const escaped = document.createElement('in\put').value;
wrong.trim(); other.trim(); dynamic.trim(); spread.trim(); fifth.trim(); unknown.trim(); escaped.trim();
}"#,
            );
            workspace.write(
                format!("property_call.{extension}"),
                "function read() { const input = document.createElement('input'); input.value(); document.createElement('input').value(); document.createElement('script').src(); }",
            );
            workspace.write(
                format!("property_alias.{extension}"),
                "function read() { const v = document.createElement('input').value; const s = document.createElement('script').src; const alias = v; v(); s(); alias(); }",
            );
            workspace.write(format!("property_shadow.{extension}"), "function read(document) { const input = document.createElement('input'); const value = input.value; const direct = document.createElement('input').value; value.trim(); direct.trim(); }");
            workspace.write(
                format!("property_write.{extension}"),
                "function read() { document = {}; const value = document.createElement('input').value; value.trim(); }",
            );
            workspace.write(format!("property_local.{extension}"), "class LocalElement { value = 'local'; } function make() { return new LocalElement(); } function read() { const item = make(); const a = item.value; const b = make().value; }\nfunction call() { const item = make(); item.value(); }");
        }
        let mut rules = Vec::new();
        for language in ["javascript", "typescript"] {
            for (id, global, method, literal, name, property) in [
                (
                    "original",
                    "nodeFactory",
                    "make",
                    "field",
                    "PropertyProof",
                    "payload",
                ),
                (
                    "renamed",
                    "viewFactory",
                    "forge",
                    "cell",
                    "RenamedProof",
                    "content",
                ),
            ] {
                rules.push(serde_json::json!({
                    "id":format!("{id}-{language}"), "language":language,
                    "operation":{"kind":"receiver_type", "receiver":{
                        "name":name, "members":[], "properties":BTreeMap::from([(property,"string")]),
                        "callback_callees":[], "generic":false,
                        "argument_factories":[{"callee":{"kind":"global_member", "receiver":global, "member":method}, "argument":0, "literals":[literal]}]
                    }}
                }));
                let extension = if language == "javascript" { "js" } else { "ts" };
                workspace.write(format!("{id}.{extension}"), &format!(
                "function read() {{ const item = {global}.{method}('{literal}'); const a = item.{property}; const b = {global}.{method}('{literal}').{property}; a.trim(); b.trim(); }}\nfunction call() {{ {global}.{method}('{literal}').{property}(); }}"
            ));
            }
        }
        let pack = serde_json::json!({"syntax_rules":rules});
        let manifest_source = toml::to_string(&pack).unwrap();
        workspace.write(MANIFEST, &manifest_source);

        let mut disabled = pack.clone();
        for rule in disabled["syntax_rules"].as_array_mut().unwrap() {
            rule["operation"]["receiver"]["argument_factories"] = serde_json::json!([]);
        }
        let disabled_source = toml::to_string(&disabled).unwrap();
        workspace.build();
        Self {
            workspace,
            manifest_source,
            disabled_source,
        }
    }

    fn disable(&self) {
        self.workspace.write(MANIFEST, &self.disabled_source);
        self.workspace.delta(&[MANIFEST]);
    }

    fn restore(&self) {
        self.workspace.write(MANIFEST, &self.manifest_source);
        self.workspace.delta(&[MANIFEST]);
    }
}

fn baseline() -> &'static PersistedSnapshot {
    static BASELINE: std::sync::OnceLock<PersistedSnapshot> = std::sync::OnceLock::new();
    BASELINE.get_or_init(|| {
        let fixture = FactoryWorkspace::new();
        PersistedSnapshot::from_connection(&fixture.workspace.open())
    })
}

fn configured_sources() -> Vec<(String, String, String, String, String)> {
    let mut sources = Vec::new();
    for extension in ["ts", "js"] {
        for (id, global, method, literal, property) in [
            ("original", "nodeFactory", "make", "field", "payload"),
            ("renamed", "viewFactory", "forge", "cell", "content"),
        ] {
            sources.push((
                format!("{id}.{extension}"),
                format!("item.{property}"),
                format!("{global}.{method}('{literal}').{property}"),
                id.into(),
                format!("{global}.{method}"),
            ));
        }
    }
    sources
}

#[test]
fn factory_origin_and_member_callability() {
    let conn = baseline().open();
    for (path, line, expression, expected, evidence) in [
        (
            "factory.ts",
            None,
            "button.addEventListener",
            "external",
            "builtin:web_api",
        ),
        (
            "factory.ts",
            None,
            "alias.addEventListener",
            "external",
            "builtin:web_api",
        ),
        (
            "factory.ts",
            None,
            "root.querySelector",
            "external",
            "builtin:web_api",
        ),
        (
            "factory.ts",
            None,
            "found.getAttribute",
            "external",
            "builtin:web_api",
        ),
        ("factory.ts", None, "button.unknownMethod", "unresolved", ""),
        (
            "shadow.ts",
            None,
            "button.addEventListener",
            "unresolved",
            "",
        ),
        (
            "reassigned_document.ts",
            None,
            "button.addEventListener",
            "unresolved",
            "",
        ),
        (
            "reassigned_result.ts",
            None,
            "button.addEventListener",
            "unresolved",
            "",
        ),
        (
            "unknown_factory.ts",
            None,
            "button.addEventListener",
            "unresolved",
            "",
        ),
        (
            "untyped.ts",
            None,
            "button.addEventListener",
            "unresolved",
            "",
        ),
        (
            "shadow.js",
            None,
            "document.addEventListener",
            "unresolved",
            "",
        ),
        ("shadow.js", None, "event.preventDefault", "unresolved", ""),
        ("local.ts", None, "button.addEventListener", "resolved", ""),
        ("literal.ts", None, "input.value.trim", "external", ""),
        ("literal.ts", None, "script.src.trim", "external", ""),
        (
            "literal.ts",
            None,
            "document.createElement('input').value.trim",
            "external",
            "",
        ),
        (
            "literal.ts",
            None,
            "document.createElement('script').src.trim",
            "external",
            "",
        ),
        ("literal.ts", None, "input.src.trim", "unresolved", ""),
        ("literal.ts", None, "script.value.trim", "unresolved", ""),
        ("literal.ts", None, "input.invented", "unresolved", ""),
        ("literal.ts", Some(2), "input.value", "unresolved", ""),
        ("literal.js", None, "input.value.trim", "external", ""),
        ("literal.js", None, "script.src.trim", "external", ""),
        (
            "literal.js",
            None,
            "document.createElement('input').value.trim",
            "external",
            "",
        ),
        (
            "literal.js",
            None,
            "document.createElement('script').src.trim",
            "external",
            "",
        ),
        ("literal.js", None, "input.src.trim", "unresolved", ""),
        ("literal.js", None, "script.value.trim", "unresolved", ""),
        ("literal.js", None, "input.invented", "unresolved", ""),
        ("literal.js", Some(2), "input.value", "unresolved", ""),
        (
            "literal_shadow.ts",
            None,
            "input.value.trim",
            "unresolved",
            "",
        ),
        (
            "literal_shadow.ts",
            None,
            "document.createElement('script').src.trim",
            "unresolved",
            "",
        ),
        (
            "literal_write.ts",
            None,
            "input.value.trim",
            "unresolved",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "dynamic.addEventListener",
            "external",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "dynamic.value.trim",
            "unresolved",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "missing.addEventListener",
            "external",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "spread.addEventListener",
            "external",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "spread.value.trim",
            "unresolved",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "excess.addEventListener",
            "external",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "excess.value.trim",
            "unresolved",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "unknown.addEventListener",
            "external",
            "",
        ),
        (
            "literal_unknown.ts",
            None,
            "unknown.value.trim",
            "unresolved",
            "",
        ),
    ] {
        let row = persisted_coverage(&conn, path, line, expression);
        assert_eq!(row.0, expected, "{path}:{expression}: {row:?}");
        if !evidence.is_empty() {
            assert!(row.1.contains(evidence), "{path}:{expression}: {row:?}");
        }
    }
    let local_edge: i64 = conn.query_row(
        "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)='local.ts' AND e.kind='calls' AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)='local.ts' AND dst.kind='method' AND dst.name='addEventListener'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(local_edge, 1);
}

#[test]
fn property_read_provenance_and_noncallability() {
    let conn = baseline().open();
    for (extension, language) in [("ts", "typescript"), ("js", "javascript")] {
        for (prefix, line, expression, expected, provenance) in [
            ("property", None, "input.value", "external", true),
            ("property", None, "script.src", "external", true),
            (
                "property",
                None,
                "document.createElement('input').value",
                "external",
                true,
            ),
            (
                "property",
                None,
                "document.createElement('script').src",
                "external",
                true,
            ),
            ("property", None, "a.trim", "external", false),
            ("property", None, "b.trim", "external", false),
            ("property", None, "c.trim", "external", false),
            ("property", None, "d.trim", "external", false),
            (
                "property",
                None,
                "document.createElement('input').src",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement('script').value",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement(tag).value",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement(...['input']).value",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement('input', 1, 2, 3, 4).value",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement('unmapped').value",
                "unresolved",
                false,
            ),
            (
                "property",
                None,
                "document.createElement('in\\put').value",
                "unresolved",
                false,
            ),
            ("property", None, "wrong.trim", "unresolved", false),
            ("property", None, "other.trim", "unresolved", false),
            ("property", None, "dynamic.trim", "unresolved", false),
            ("property", None, "spread.trim", "unresolved", false),
            ("property", None, "fifth.trim", "unresolved", false),
            ("property", None, "unknown.trim", "unresolved", false),
            ("property", None, "escaped.trim", "unresolved", false),
            ("property_alias", None, "v", "unresolved", false),
            ("property_alias", None, "s", "unresolved", false),
            ("property_alias", None, "alias", "unresolved", false),
            ("property_call", None, "input.value", "unresolved", false),
            (
                "property_call",
                None,
                "document.createElement('input').value",
                "unresolved",
                false,
            ),
            (
                "property_call",
                None,
                "document.createElement('script').src",
                "unresolved",
                false,
            ),
            (
                "property_shadow",
                None,
                "document.createElement('input').value",
                "unresolved",
                false,
            ),
            ("property_shadow", None, "value.trim", "unresolved", false),
            ("property_shadow", None, "direct.trim", "unresolved", false),
            (
                "property_write",
                None,
                "document.createElement('input').value",
                "unresolved",
                false,
            ),
            ("property_write", None, "value.trim", "unresolved", false),
            ("property_local", Some(1), "item.value", "resolved", false),
            ("property_local", Some(1), "make().value", "resolved", false),
            ("property_local", Some(2), "item.value", "unresolved", false),
        ] {
            let path = format!("{prefix}.{extension}");
            let row = persisted_coverage(&conn, &path, line, expression);
            assert_eq!(row.0, expected, "{path}:{expression}: {row:?}");
            if provenance {
                for token in [
                    format!("finite {language} configured member"),
                    "pack web".into(),
                    "receiver rule literal-".into(),
                    "receiver origin external import document.createElement".into(),
                ] {
                    assert!(
                        row.1.contains(&token),
                        "{path}:{expression}: missing {token}: {row:?}"
                    );
                }
            }
        }
        let shadow_path = format!("property_shadow.{extension}");
        let absent: i64 = conn.query_row(
            "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)=?1 AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='input.value'",
            [&shadow_path], |row| row.get(0),
        ).unwrap();
        assert_eq!(absent, 0);
        let edges: i64 = conn
            .query_row("SELECT count(*) FROM edge_occurrences WHERE (SELECT path FROM path_dictionary WHERE path_id=owner_id)=?1 AND kind IN ('calls','references')", [&shadow_path], |row| row.get(0))
            .unwrap();
        assert_eq!(edges, 0);

        let local_path = format!("property_local.{extension}");
        let local_targets: i64 = conn.query_row(
            "SELECT count(*) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)=?1 AND e.kind='references' AND (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)=?1 AND dst.kind='field' AND dst.name='value'",
            [&local_path], |row| row.get(0),
        ).unwrap();
        assert_eq!(local_targets, 2);
    }
}

#[test]
fn configured_property_disable_restore() {
    let fixture = FactoryWorkspace::new();
    for (phase, expected) in [(0, "external"), (1, "unresolved"), (2, "external")] {
        match phase {
            0 => (),
            1 => fixture.disable(),
            _ => fixture.restore(),
        }
        let conn = fixture.workspace.open();
        for (path, ordinary, direct, id, origin) in configured_sources() {
            for expression in [&direct, "a.trim", "b.trim"] {
                let (status, evidence) = persisted_coverage(&conn, &path, Some(1), expression);
                assert_eq!(status, expected, "{path}: {expression}: {evidence}");
                if expected == "external" && expression == direct {
                    assert!(evidence.contains("pack property-proof"));
                    assert!(evidence.contains(&format!("receiver rule {id}-")));
                    assert!(evidence.contains(&format!("receiver origin external import {origin}")));
                }
            }
            if expected == "external" {
                let (status, evidence) = persisted_coverage(&conn, &path, Some(1), &ordinary);
                assert_eq!(status, "external");
                assert!(evidence.contains("pack property-proof"));
                assert!(evidence.contains(&format!("receiver rule {id}-")));
                assert!(evidence.contains(&format!("receiver origin external import {origin}")));
            }
        }
        if phase == 1 {
            for (path, ordinary, _, _, _) in configured_sources() {
                let absent: i64 = conn.query_row(
            "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)=?1 AND line=1 AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)=?2",
            rusqlite::params![path, ordinary], |r| r.get(0),
        ).unwrap();
                assert_eq!(
                    absent, 0,
                    "disabled mapping has no ordinary property observation: {path}"
                );
                let targets: i64 = conn
            .query_row("SELECT count(*) FROM edge_occurrences WHERE (SELECT path FROM path_dictionary WHERE path_id=owner_id)=?1 AND kind IN ('calls','references')", [&path], |r| r.get(0))
            .unwrap();
                assert_eq!(targets, 0, "disabled mapping has no success target: {path}");
            }
        }
        for (path, _, direct, _, _) in configured_sources() {
            assert_eq!(
                persisted_coverage(&conn, &path, Some(2), &direct).0,
                "unresolved",
                "{path}: phase {phase}"
            );
        }

        let parity = crate::languages::harness::cold_relations(&fixture.workspace);
        assert_eq!(parity.incremental_verification, Ok(()));
        assert_eq!(parity.cold_verification, Ok(()));
        assert_eq!(parity.incremental, parity.cold);
    }
}
