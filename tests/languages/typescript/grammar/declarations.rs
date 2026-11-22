use super::*;
#[test]
fn receiver_binding_contract_matrix() {
    for (source, expression, expected) in [
("class Worker { work() {} } function run() { const client = new Worker(); client.work(); }", "client.work", true),
("class Worker { work() {} } function run() { const client: Worker = new Worker(); const alias = client; alias.work(); }", "alias.work", true),
("function run() { const client = {work() {}}; client.work(); }", "client.work", true),
("class Worker { work() {} } function run() { const client = new Worker(); client.work(); }", "client.work", true),
("class Worker { work() {} } function run() { let client: Worker; client.work(); }", "client.work", false),
("class Worker { work() {} } function run() { const client: Worker = client.work(); }", "client.work", false)] {
        assert_eq!(resolves(source, expression), expected, "{source}: {expression}");
    }
}

#[test]
fn inherited_methods_select_actual_overrides_and_reject_parent_cycles() {
    assert!(resolves(
        "class Base { work() {} } class Worker extends Base {} function run() { const client = new Worker(); client.work(); }",
        "client.work"
    ));
    let source = "class Base { work() {} } class Worker extends Base { work() {} } function run() { const client = new Worker(); client.work(); }";
    let graph = linked("service.ts", "typescript", source);
    let facts = &graph.facts;
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.kind == "calls" && edge.evidence == "client.work"));
    let expected = facts
        .nodes
        .iter()
        .find(|node| node.qualname.ends_with(".Worker.work"))
        .unwrap()
        .id
        .clone();
    assert!(graph.edges.iter().any(|edge| edge.kind == "calls"
        && edge.evidence == "client.work"
        && edge.dst == expected));
    assert!(!resolves(
        "class A extends B {} class B extends A { work() {} } function run() { const client = new A(); client.work(); }",
        "client.work"
    ));
}

#[test]
fn declaration_type_contract_matrix() {
    let mut cases = Vec::new();
    for (source, expression) in [
        (
            "class Worker { work() {} }\n/** @param {Worker} client */\nfunction run(client) { client.work(); }",
            "client.work",
        ),
        (
            "class Worker { work() {} }\n/** @returns {Worker} */\nfunction make() { return new Worker(); }\nfunction run() { const client = make(); client.work(); }",
            "client.work",
        ),
    ] {
        cases.push(("service.js", "javascript", source.to_owned(), vec![expression], Vec::new()));
    }
    for (path, language, declaration, expected) in [
        (
            "service.js",
            "javascript",
            "/**\n * @param {Worker} client\n * @returns {Worker}\n */\nfunction make(client) { client.work(); return client; }",
            "resolved",
        ),
        (
            "service.ts",
            "typescript",
            "/**\n * @param {Other} client\n * @returns {Other}\n */\nfunction make(client: Worker): Worker { client.work(); return client; }",
            "resolved",
        ),
        (
            "service.js",
            "javascript",
            "/**\n * @param {Worker} client\n * @returns {Worker}\n * @returns {Other}\n */\nfunction make(client) { client.work(); return client; }",
            "unresolved",
        ),
        (
            "service.js",
            "javascript",
            "/**\n * @param {Worker} client\n * @returns {Worker}\n */\nasync function make(client) { client.work(); return client; }",
            "unresolved",
        ),
    ] {
        cases.push((path, language, format!("class Worker {{ work() {{}} }} class Other {{}}\n{declaration}\nconst output = make(new Worker()); output.work();"), Vec::new(), vec![("client.work", "resolved"), ("output.work", expected)]));
    }
    for (path, language, source, calls, coverage) in cases {
        let graph = linked(path, language, &source);
        for expression in calls {
            assert!(
                graph
                    .edges
                    .iter()
                    .any(|edge| edge.kind == "calls" && edge.evidence == expression),
                "{source}: {expression}"
            );
        }
        for (expression, status) in coverage {
            let rows: Vec<_> = graph
                .coverage
                .iter()
                .filter(|row| row.expression == expression)
                .collect();
            assert!(!rows.is_empty(), "{source}: {expression}");
            assert!(
                rows.iter().all(|row| row.status == status),
                "{source}: {rows:#?}"
            );
        }
    }
}

#[test]
fn factory_return_contract_matrix() {
    let mut cases: Vec<(String, &str, bool)> = [
("class Worker { work() {} } function run() { (new Worker()).work(); }", "(new Worker()).work", true),
("class Worker { work() {} } function make(): Worker { return new Worker(); } function run() { make().work(); }", "make().work", true),
("class Worker { work() {} } function run() { Worker().work(); }", "Worker().work", false),
("class Worker { work() {} } function make() { return new Worker(); } function wrap() { const result = make(); return result; } function run() { const client = wrap(); client.work(); }", "client.work", true),
("class Worker { work() {} } const make = () => new Worker(); function run() { const client = make(); client.work(); }", "client.work", true),
("class Worker { work() {} } function make() { return wrap(); } function wrap() { return make(); } function run() { const client = make(); client.work(); }", "client.work", false),
("class Worker { work() {} } function make(flag) { if (flag) return new Worker(); return unknown(); } function run() { const client = make(true); client.work(); }", "client.work", false),
("class Worker { work() {} } function make(flag) { if (flag) return new Worker(); } function run() { const client = make(true); client.work(); }", "client.work", false),
("class Worker { work() {} } function make() { return new Worker(); } function run() { const client = make(); client.work(); }", "client.work", true)].into_iter().map(|(source, expression, expected)| (source.to_owned(), expression, expected)).collect();
    cases.extend([
        "async function make() { return new Worker(); }",
        "async function make(): Worker { return new Worker(); }",
        "const make = async () => new Worker();",
        "function* make(): Worker { return new Worker(); }",
        "class Factory { static async make(): Worker { return new Worker(); } } const make = Factory.make;",
    ].into_iter().map(|factory| (format!("class Worker {{ work() {{}} }} {factory} function run() {{ const client = make(); client.work(); }}"), "client.work", false)));
    for (source, expression, expected) in cases {
        assert_eq!(
            resolves(&source, expression),
            expected,
            "{source}: {expression}"
        );
    }
}

#[test]
fn generic_containers_prove_only_documented_container_members() {
    let members = "push pop shift unshift slice splice indexOf includes join map filter forEach reduce find findIndex some every flat flatMap";
    let member_calls = members
        .split_whitespace()
        .map(|member| format!("items.{member}();"))
        .collect::<String>();
    for annotation in ["Array<Worker>", "Worker[]", "Array<Array<Worker>>"] {
        let source = format!(
            "class Worker {{ work() {{}} }} function run() {{ const items: {annotation} = []; {member_calls} items.work(); }}"
        );
        let graph = linked("generic.ts", "typescript", &source);
        for member in members.split_whitespace() {
            assert!(
                graph
                    .coverage(&format!("items.{member}"))
                    .any(|row| row.status == "external"
                        && row.evidence.contains("JavaScript/Web platform")),
                "{annotation}: {member}"
            );
        }
        assert!(!graph.edges.iter().any(|edge| edge.evidence == "items.work"));
    }
    let source =
        "class Array<T> { custom() {} } function run() { const items: Array<string> = unknown(); items.map(); }";
    let graph = linked("shadow.ts", "typescript", source);
    assert!(graph.statuses("items.map").contains(&"unresolved"));
    let source = "function Array<T>() { return unknown(); } function run() { const items: Array<string> = unknown(); items.map(); }";
    let graph = linked("function-shadow.ts", "typescript", source);
    assert!(graph.statuses("items.map").contains(&"unresolved"));
}

#[test]
fn static_dispatch_contract_matrix() {
    let source = "class Worker { instance() {} static associated() {} static runStatic() { this.instance(); this.associated(); } runInstance() { this.instance(); this.associated(); } }";
    let graph = linked("static.ts", "typescript", source);
    let facts = &graph.facts;
    for edge in graph.edges.iter().filter(|edge| edge.kind == "calls") {
        let owner = facts.nodes.iter().find(|node| node.id == edge.src).unwrap();
        let target = facts.nodes.iter().find(|node| node.id == edge.dst).unwrap();
        assert_eq!(
            owner.details["is_static"], target.details["is_static"],
            "{} -> {}",
            owner.qualname, target.qualname
        );
    }
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.kind == "calls")
            .count(),
        2
    );
    for (source, expression, expected) in [
("class Worker { work() {} make(): Worker { return new Worker(); } } function run() { const client = Worker.make(); client.work(); }", "client.work", false),
("class Worker { work() {} static make(): Worker { return new Worker(); } } function run() { const client = Worker.make(); client.work(); }", "client.work", true)] {
        assert_eq!(resolves(source, expression), expected, "{source}");
    }
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write(
        "view.ts",
        r#"
class View {
  instanceMethod(): HTMLElement {
    return document.createElement("div");
  }
  static make(): HTMLElement {
    return document.createElement("div");
  }
  static run() {
    const leaked = this.instanceMethod();
    leaked.appendChild(document.createElement("span"));
    const built = this.make();
    built.appendChild(document.createElement("span"));
  }
}
"#,
    );
    workspace.build();
    let conn = workspace.open();
    for (expression, expected, origin) in [
        ("leaked.appendChild", "unresolved", None),
        ("built.appendChild", "external", Some("builtin:web_api")),
    ] {
        let (status, evidence) = persisted_coverage(&conn, "view.ts", None, expression);
        assert_eq!(status, expected, "{expression}: {evidence}");
        if let Some(origin) = origin {
            assert!(evidence.contains(origin), "{expression}: {evidence}");
        }
    }
    drop(conn);
}

#[test]
fn typescript_optional_call_keeps_its_persisted_expression() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write(
        "optional.ts",
        "const client: { run(): void } = { run() {} };\nclient?.run();",
    );
    workspace.build();
    let conn = workspace.open();
    let expressions: Vec<String> = conn.prepare(
        "SELECT (SELECT expression FROM coverage_expressions WHERE expression_id=c.expression_id) FROM resolution_coverage c WHERE (SELECT path FROM path_dictionary WHERE path_id=c.path_id)='optional.ts' AND c.line=2",
    ).unwrap().query_map([], |row| row.get(0)).unwrap()
        .map(Result::unwrap).collect();
    assert!(
        expressions
            .iter()
            .any(|expression| expression == "client?.run"),
        "TypeScript keeps the original optional call expression: {expressions:?}"
    );
    assert!(
        !expressions
            .iter()
            .any(|expression| expression == "client.run"),
        "JavaScript-only normalization must not rewrite TypeScript coverage: {expressions:?}"
    );
    drop(conn);
}

#[test]
fn finite_standard_static_members_require_unshadowed_globals() {
    for (source, expected) in [
        (
            "function run() { Number.isFinite(1); Reflect.get({}, 'x'); crypto.randomUUID(); document.createElement('div'); window.addEventListener('click', () => {}); }",
            "external",
        ),
        (
            "function run(Number, Reflect, crypto, document, window) { Number.isFinite(1); Reflect.get({}, 'x'); crypto.randomUUID(); document.createElement('div'); window.addEventListener('click', () => {}); }",
            "unresolved",
        ),
    ] {
        let graph = linked("static.ts", "typescript", source);
        for expression in [
            "Number.isFinite",
            "Reflect.get",
            "crypto.randomUUID",
            "document.createElement",
            "window.addEventListener",
        ] {
            assert!(
                graph
                    .coverage
                    .iter()
                    .any(|row| row.expression == expression && row.status == expected),
                "{expression}: {graph:#?}"
            );
        }
    }
    let graph = ExtractorHarness::for_lang("typescript")
        .at_path("comment-require.ts")
        .linked("function run(document) { document.createElement('div'); } // require");
    assert!(
        graph.coverage.iter().any(|row| {
            row.expression == "document.createElement" && row.status == "unresolved"
        }),
        "shadowed document must stay unresolved even when a comment mentions require: {graph:#?}"
    );
    let graph = ExtractorHarness::for_lang("typescript")
        .at_path("unknown.ts")
        .linked("Number.noSuchMember(); Reflect.noSuchMember(); crypto.noSuchMember();");
    for expression in [
        "Number.noSuchMember",
        "Reflect.noSuchMember",
        "crypto.noSuchMember",
    ] {
        assert!(
            graph
                .coverage
                .iter()
                .any(|row| row.expression == expression && row.status == "unresolved"),
            "{expression}: {graph:#?}"
        );
    }
}

#[test]
fn persisted_constructor_field_matrix() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write(
        "table.ts",
        r#"
class TableView {
  constructor(element: HTMLElement) {
    this.tbody = element;
    this.activeRow = null;
  }
  render() {
    this.tbody.appendChild(document.createElement("tr"));
    this.activeRow.select();
  }
}
"#,
    );
    workspace.write(
        "later.ts",
        r#"
class LaterTableView {
  constructor(element: HTMLElement) {
    this.tbody = element;
  }
  reset(element: HTMLElement) {
    this.tbody = element;
  }
  touch() {
    this.tbody += 1;
  }
  render() {
    this.tbody.appendChild(document.createElement("tr"));
  }
}
"#,
    );
    workspace.build();
    let conn = workspace.open();
    // Verify tbody and activeRow are indexed as fields under TableView
    let mut stmt = conn.prepare("SELECT name, kind, qualname FROM nodes WHERE path_id=(SELECT path_id FROM path_dictionary WHERE path='table.ts') AND kind='field' ORDER BY name").unwrap();
    let fields: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert_eq!(
        fields,
        vec![
            (
                "activeRow".to_string(),
                "field".to_string(),
                "table.TableView.activeRow".to_string()
            ),
            (
                "tbody".to_string(),
                "field".to_string(),
                "table.TableView.tbody".to_string()
            ),
        ],
        "fields in nodes table: {fields:?}"
    );

    for (path, expression, expected, origin) in [
        (
            "table.ts",
            "this.tbody.appendChild",
            "external",
            Some("builtin:web_api"),
        ),
        ("table.ts", "this.activeRow.select", "unresolved", None),
        (
            "later.ts",
            "this.tbody.appendChild",
            "external",
            Some("builtin:web_api"),
        ),
    ] {
        let (status, evidence) = persisted_coverage(&conn, path, None, expression);
        assert_eq!(status, expected, "{path}: {expression}: {evidence}");
        if let Some(origin) = origin {
            assert!(evidence.contains(origin), "{path}: {evidence}");
        }
    }
    drop(stmt);
    drop(conn);
}

#[test]
fn typescript_destructuring_callback_and_interface_polymorphism_matrix() {
    let w = crate::languages::PersistedWorkspaceHarness::new();
    let mut overflow_ts = String::from("interface ManyRenderer { render(): void; }\n");
    for idx in 0..9 {
        overflow_ts.push_str(&format!(
            "class Impl{idx} implements ManyRenderer {{ render() {{}} }}\n"
        ));
    }
    overflow_ts.push_str("function drawMany(m: ManyRenderer) { m.render(); }\n");
    w.write("overflow.ts", &overflow_ts);
    w.write(
        "app.ts",
        r#"
class User { save() {} }
class Ctx { user: User; constructor() { this.user = new User(); } }
function withUser(cb: (u: User) => void) {}
function run() {
  const { user } = new Ctx();
  user.save();
  withUser((cbUser) => cbUser.save());
}
interface Renderer { render(): void; }
class HtmlRenderer implements Renderer { render() {} }
class JsonRenderer implements Renderer { render() {} }
function draw(r: Renderer) { r.render(); }
"#,
    );
    w.build();
    let conn = w.open();
    for expr in ["user.save", "cbUser.save"] {
        let (status, evidence): (String, String) = conn
            .query_row(
                "SELECT c.status, v.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence v ON v.evidence_id=c.evidence_id WHERE p.path='app.ts' AND x.expression=?1",
                [expr],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("missing {expr}: {e}"));
        assert_eq!(status, "resolved", "app.ts: {expr} ({evidence})");
    }
    let bounded_possible: i64 = conn
        .query_row(
            "SELECT count(*) FROM edge_occurrences e JOIN path_dictionary p ON p.path_id=e.owner_id JOIN coverage_evidence c ON c.evidence_id=e.confidence_id WHERE p.path='app.ts' AND e.kind='possible_calls' AND c.evidence='possible'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bounded_possible, 2, "Renderer has 2 concrete implementors");
    let overflow_possible: i64 = conn
        .query_row(
            "SELECT count(*) FROM edge_occurrences e JOIN path_dictionary p ON p.path_id=e.owner_id WHERE p.path='overflow.ts' AND e.kind='possible_calls'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(overflow_possible, 0, ">8 implementors must widen cleanly");
    let (overflow_status, overflow_ev): (String, String) = conn
        .query_row(
            "SELECT c.status, v.evidence FROM resolution_coverage c JOIN path_dictionary p ON p.path_id=c.path_id JOIN coverage_expressions x ON x.expression_id=c.expression_id JOIN coverage_evidence v ON v.evidence_id=c.evidence_id WHERE p.path='overflow.ts' AND x.expression='m.render'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(overflow_status, "unresolved", "{overflow_ev}");
    assert!(overflow_ev.contains("Unknown/Incomplete"), "{overflow_ev}");
}
