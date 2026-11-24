use crate::languages::harness::LinkedCatalog;
use std::sync::OnceLock;

fn factory_catalog() -> &'static LinkedCatalog {
    static CATALOG: OnceLock<LinkedCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let main_source = r#"
mod common;
mod acdd;
mod rev7_client;
use rev7_client::StdioClientRev7;
use acdd::support::DEFAULT_SEAM;
use common::Workspace;

fn probe_rev7_gaps() {
    let workspace = acdd::support::acdd_workspace(DEFAULT_SEAM, "");
    workspace.write("a.rs", "");
    workspace.root().join("child");
    let projections = (workspace.root(), &workspace);
    projections.0.join("child");
    projections.1.write("wrapped.rs", "");

    let b = Workspace::new();
    let r = Workspace::new();
    for w in [&b, &r] {
        w.write("loop.rs", "");
    }
    let pair = (&b, &r);
    let (first, second) = pair;
    first.write("first.rs", "");
    second.write("second.rs", "");

    let mut cmd = std::process::Command::new("git");
    let output = cmd.current_dir(".").output().unwrap();
    output.status.success();
    output.stdout.len();
    let json = serde_json::from_str::<serde_json::Value>("{}").unwrap();
    json.as_object();
    let yaml = serde_yaml::from_str::<serde_yaml::Value>("{}").unwrap();
    yaml.as_mapping();
}
fn unknown_array(factory: impl Fn() -> Workspace) {
    let known = Workspace::new();
    for unknown in [&known, &factory()] {
        unknown.write("unknown.rs", "");
    }
}
fn database(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    let mut statement = conn.prepare("SELECT 1")?;
    statement.query([])?;
    let mut unwrapped = conn.prepare("SELECT 1").unwrap();
    unwrapped.query([]).unwrap();
    Ok(())
}
fn transaction(tx: &rusqlite::Transaction<'_>) {
    tx.query_row("SELECT 1", [], |transaction_row| transaction_row.get::<_, i64>(0));
}
struct Outer {}
impl Outer { fn outer(&self) {} }
struct Inner {}
impl Inner { fn inner(&self) {} }
fn lexical(flag: bool) {
    let value = Outer {};
    if flag {
        let value = Inner {};
        value.inner();
        let (tuple_local, _) = (Outer {}, ());
        tuple_local.outer();
    }
    value.outer();
    for _ in [1] {
        let nested = Outer {};
        nested.outer();
    }
    let owned = "known".to_owned();
    "known".len();
    owned.len();
    std::path::Path::new(".").join("child").exists();
    String::from_utf8_lossy(&[]).len();
    u32::try_from(1u64);
}
struct GenericHolder<'a> { leaf: LeafRev7, label: &'a str }
struct LeafRev7 {}
impl LeafRev7 { fn finish(&self) {} }
impl<'a> GenericHolder<'a> { fn run(&self) { self.leaf.finish(); } }
type ClientRev7 = StdioClientRev7;
type ChainedClientRev7 = ClientRev7;
type CycleARev7 = CycleBRev7;
type CycleBRev7 = CycleARev7;
fn next_gaps(response: serde_json::Value, metadata: std::fs::Metadata, unrelated: UnknownRev7) {
    let client = ChainedClientRev7::new();
    client.payload();
    response["result"]["tools"].as_array();
    let indexed = response["result"]["tools"];
    indexed.as_array();
    unrelated["result"].as_array();
    CycleARev7::new();
    metadata.file_type().is_file();
    std::time::Instant::now().elapsed().as_millis();
    tempfile::Builder::new().prefix("probe").tempdir().unwrap().path();
}
struct BatchFacts<'a> { nodes: Vec<LeafRev7>, leaf: &'a LeafRev7 }
impl<'a> BatchFacts<'a> {
    fn shared(&self) {
        let this = self;
        this.leaf.finish();
        self.nodes.iter().next().unwrap().finish();
    }
    fn mutable(&mut self) {
        let that = self;
        that.nodes.iter_mut().next().unwrap().finish();
    }
}
fn batch_parameters<'a>(plain: &LeafRev7, mutable: &mut LeafRev7, named: &'a LeafRev7,
    anonymous: &'_ LeafRev7, facts: &BatchFacts<'a>, path: &'a std::path::Path,
    nodes: &[LeafRev7], optional: &Option<Vec<LeafRev7>>, result: &Result<LeafRev7, ()>) {
    plain.finish(); mutable.finish(); named.finish(); anonymous.finish();
    facts.leaf.finish();
    facts.nodes.iter().next().unwrap().finish();
    nodes.into_iter().next().unwrap().finish();
    optional.as_deref().unwrap().iter().next().unwrap().finish();
    result.as_ref().unwrap().finish();
    path.join("child").exists();
    let collection: Vec<LeafRev7> = Vec::new();
    collection.as_ref().iter().next().unwrap().finish();
}
fn batch_unknown<T>(unknown: &T) { unknown.finish(); }
struct BatchMaker {}
impl BatchMaker {
    fn current() -> Self { Self {} }
    fn ready(&self) {}
    fn new() -> LeafRev7 { LeafRev7 {} }
}
fn batch_tuple() -> (LeafRev7, Outer) { (LeafRev7 {}, Outer {}) }
struct MissingConstructor {}
impl MissingConstructor { fn finish(&self) {} }
struct __rust_closure {}
fn batch_six() {
    let maker = BatchMaker::current(); maker.ready();
    let changed = BatchMaker::new(); changed.finish();
    let (tuple_leaf, mut tuple_outer) = batch_tuple();
    tuple_leaf.finish(); tuple_outer.outer();
    let git = |args: &[&str]| {};
    git(&["status"]);
    let invoke = || LeafRev7 {};
    let again = invoke;
    invoke(); again();
    let unproven_result = invoke(); unproven_result.finish();
    unproven_result();
    let missing = MissingConstructor::new(); missing.finish();
    let fake = __rust_closure {}; fake();
    let number = 1; number();
}
struct BatchHub {}
impl BatchHub { fn leaf(&self) -> LeafRev7 { LeafRev7 {} } }
fn make_hub() -> BatchHub { BatchHub {} }
fn result_hub() -> Result<BatchHub, ()> { Ok(BatchHub {}) }
struct Collision { finish: LeafRev7 }
impl Collision { fn finish(&self) {} fn inspect(&self) { let _ = &self.finish; self.finish(); } }
fn batch_seven<'a>(facts: &BatchFacts<'a>, collision: &Collision, name: &str, target: &std::path::PathBuf) -> Result<(), ()> {
    let _ = &facts.nodes;
    let _ = &facts.leaf;
    let _ = &collision.finish;
    collision.finish();
    facts.leaf();
    make_hub().leaf().finish();
    result_hub()?.leaf().finish();
    name.trim().is_empty();
    target.parent().unwrap().exists();
    std::env::temp_dir().join("child").exists();
    Ok(())
}
"#;
        let sources = &[
            (
                "tests/common/mod.rs",
                "#[path = \"workspace.rs\"] mod workspace; pub use workspace::Workspace;",
            ),
            (
                "tests/common/workspace.rs",
                include_str!("../../common/workspace.rs"),
            ),
            (
                "tests/acdd/mod.rs",
                "pub(crate) mod support; mod tuple_probe;",
            ),
            (
                "tests/acdd/support.rs",
                "use crate::common::Workspace;\npub(crate) const DEFAULT_SEAM: &str = \"tests/seam.rs\";\npub(crate) fn acdd_workspace(_seam: &str, _target: &str) -> Workspace { Workspace::new() }\npub(crate) fn fixture(_seam: &str) -> (Workspace, (), ()) { (Workspace::new(), (), ()) }",
            ),
            (
                "tests/acdd/tuple_probe.rs",
                "use super::support::*; fn probe() { let (root, _, _) = fixture(DEFAULT_SEAM); root.write(\"tuple.rs\", \"\"); root.root().join(\"child\"); }",
            ),
            (
                "tests/rev7_client.rs",
                "pub struct StdioClientRev7 {} impl StdioClientRev7 { pub fn new() -> Self { Self {} } pub fn payload(&self) {} }",
            ),
            ("tests/rev7_gap_probe.rs", main_source),
        ];
        let facts = sources
            .iter()
            .map(|(path, source)| {
                let parsed = acdd_index::ast::extract(path, "rust", source)
                    .unwrap_or_else(|error| panic!("{path} (rust): {error}"));
                ((*path).to_owned(), parsed)
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let registry_root = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
        let registry = acdd_index::languages::dependency_registry::DependencyRegistry::collect(registry_root);
        let graph = acdd_index::linker::link_with_registry(&facts, None, None, &registry);
        LinkedCatalog { facts, graph }
    })
}

#[test]
fn factory_value_flow_status() {
    let catalog = factory_catalog();
    struct Case {
        case_id: &'static str,
        path: &'static str,
        expected: &'static [(&'static str, &'static str)],
    }
    let cases = [
        Case {
            case_id: "acdd_workspace_factory_propagates_write_and_tuple_projections",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("workspace.write", "resolved"),
                ("workspace.root", "resolved"),
                ("workspace.root().join", "external"),
                ("projections.0.join", "external"),
                ("projections.1.write", "resolved"),
            ],
        },
        Case {
            case_id: "loop_pair_and_command_receivers_stay_bounded",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("w.write", "bounded"),
                ("first.write", "bounded"),
                ("second.write", "bounded"),
                ("cmd.current_dir", "bounded"),
                ("output.status.success", "bounded"),
                ("output.stdout.len", "bounded"),
                ("json.as_object", "bounded"),
                ("yaml.as_mapping", "bounded"),
            ],
        },
        Case {
            case_id: "lexical_std_generic_and_alias_receivers_stay_bounded",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("value.inner", "bounded"),
                ("value.outer", "bounded"),
                ("tuple_local.outer", "bounded"),
                ("nested.outer", "bounded"),
                ("owned.len", "bounded"),
                ("\"known\".len", "bounded"),
                (
                    "std::path::Path::new(\".\").join(\"child\").exists",
                    "bounded",
                ),
                ("String::from_utf8_lossy(&[]).len", "bounded"),
                ("u32::try_from", "bounded"),
                ("self.leaf.finish", "bounded"),
                ("ChainedClientRev7::new", "bounded"),
                ("client.payload", "bounded"),
                ("response[\"result\"][\"tools\"].as_array", "bounded"),
                ("indexed.as_array", "bounded"),
                ("metadata.file_type().is_file", "bounded"),
                ("std::time::Instant::now().elapsed().as_millis", "bounded"),
                (
                    "tempfile::Builder::new().prefix(\"probe\").tempdir().unwrap().path",
                    "bounded",
                ),
            ],
        },
        Case {
            case_id: "batch_parameter_constructor_and_hub_receivers_stay_bounded",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("this.leaf.finish", "bounded"),
                ("self.nodes.iter().next().unwrap().finish", "bounded"),
                ("that.nodes.iter_mut().next().unwrap().finish", "bounded"),
                ("plain.finish", "bounded"),
                ("mutable.finish", "bounded"),
                ("named.finish", "bounded"),
                ("anonymous.finish", "bounded"),
                ("facts.leaf.finish", "bounded"),
                ("facts.nodes.iter().next().unwrap().finish", "bounded"),
                ("nodes.into_iter().next().unwrap().finish", "bounded"),
                (
                    "optional.as_deref().unwrap().iter().next().unwrap().finish",
                    "bounded",
                ),
                ("result.as_ref().unwrap().finish", "bounded"),
                ("path.join(\"child\").exists", "bounded"),
                (
                    "collection.as_ref().iter().next().unwrap().finish",
                    "bounded",
                ),
                ("maker.ready", "bounded"),
                ("changed.finish", "bounded"),
                ("tuple_leaf.finish", "bounded"),
                ("tuple_outer.outer", "bounded"),
                ("git", "bounded"),
                ("invoke", "bounded"),
                ("again", "bounded"),
                ("self.nodes", "bounded"),
                ("self.leaf", "bounded"),
                ("collision.finish", "bounded"),
                ("make_hub().leaf().finish", "bounded"),
                ("result_hub()?.leaf().finish", "bounded"),
                ("name.trim().is_empty", "bounded"),
                ("target.parent().unwrap().exists", "bounded"),
                ("std::env::temp_dir().join(\"child\").exists", "bounded"),
                ("transaction_row.get", "bounded"),
            ],
        },
        Case {
            case_id: "unknown_cycles_and_unproven_results_fail_closed",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("unrelated[\"result\"].as_array", "unresolved"),
                ("CycleARev7::new", "unresolved"),
                ("unknown.finish", "unresolved"),
                ("unproven_result.finish", "unresolved"),
                ("unproven_result", "unresolved"),
                ("missing.finish", "unresolved"),
                ("fake", "unresolved"),
                ("number", "unresolved"),
                ("unknown.write", "unresolved"),
            ],
        },
        Case {
            case_id: "rusqlite_prepare_query_stays_external",
            path: "tests/rev7_gap_probe.rs",
            expected: &[
                ("statement.query", "external"),
                ("unwrapped.query", "external"),
            ],
        },
        Case {
            case_id: "imported_tuple_fixture_keeps_root_receiver",
            path: "tests/acdd/tuple_probe.rs",
            expected: &[
                ("root.write", "resolved"),
                ("root.root", "resolved"),
                ("root.root().join", "external"),
            ],
        },
    ];
    for Case {
        case_id,
        path,
        expected,
    } in cases
    {
        for &(expression, expected) in expected {
            let status = catalog.coverage_status(path, expression);
            if expected == "bounded" {
                assert!(
                    matches!(status, "resolved" | "external"),
                    "{case_id}: {expression}: {status}"
                );
            } else {
                assert_eq!(status, expected, "{case_id}: {expression}");
            }
        }
    }
}

#[test]
fn field_read_prefers_declared_field_over_same_name_method() {
    let catalog = factory_catalog();
    let edge_count = |source_path: &str,
                      source_name: &str,
                      edge_kind: &str,
                      target_kind: &str,
                      target_name: Option<&str>| {
        let source_ids: Vec<_> = catalog.facts[source_path]
            .nodes
            .iter()
            .filter(|node| node.name == source_name)
            .map(|node| node.id.as_str())
            .collect();
        let target_ids: Vec<_> = catalog
            .facts
            .values()
            .flat_map(|facts| &facts.nodes)
            .filter(|node| {
                node.kind.as_str() == target_kind && target_name.is_none_or(|name| node.name == name)
            })
            .map(|node| node.id.as_str())
            .collect();
        catalog
            .graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == edge_kind
                    && source_ids.contains(&edge.src.as_str())
                    && target_ids.contains(&edge.dst.as_str())
            })
            .count()
    };
    assert_eq!(
        edge_count(
            "tests/rev7_gap_probe.rs",
            "inspect",
            "references",
            "field",
            Some("finish"),
        ),
        1,
        "field reads must prefer the declared field over a same-name method"
    );
    assert_eq!(
        edge_count(
            "tests/rev7_gap_probe.rs",
            "batch_seven",
            "calls",
            "field",
            None,
        ),
        0,
        "a field read must not manufacture a callable edge"
    );
}
