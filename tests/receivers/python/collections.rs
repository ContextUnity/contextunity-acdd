use super::*;

#[test]
fn persisted_imported_typed_dict_returns_keep_finite_mapping_members() {
    let mut files = Vec::<(String, String)>::new();
    for root in ["named", "synapse"] {
        let schema = format!("from typing import TypedDict\nclass {root}Node(TypedDict, total=False):\n    name: str\n");
        let provider = format!("from . import schema as types\ndef load() -> types.{root}Node:\n    return {{'name': 'Ada'}}\n");
        let consumer = "from .provider import load\nfrom .fake import load_fake\nnode = load()\nnode.get('name')\nload().get('name')\nnode.unsupported_method()\nfake = load_fake()\nfake.get('name')\n";
        for (path, source) in [
            ("pkg/__init__.py", ""),
            ("pkg/schema.py", &schema),
            ("pkg/provider.py", &provider),
            ("pkg/fake.py", "class TypedDict: pass\nclass Fake(TypedDict): pass\ndef load_fake() -> Fake:\n    return Fake()\n"),
            ("pkg/consumer.py", consumer),
        ] { files.push((path.replacen("pkg/", &format!("{root}/"), 1), source.to_owned())); }
    }
    let sources: Vec<_> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let (_workspace, db) = build_workspace(&sources);
    for root in ["named", "synapse"] {
        let consumer_path = format!("{root}/consumer.py");
        let get = persisted_coverage(&db, &consumer_path, "node.get");
        assert_eq!(get.len(), 1, "{root}: {get:?}");
        assert_eq!(get[0].0, "external", "{root}: {get:?}");
        assert!(
            get[0].1.contains("Python standard library"),
            "{root}: {get:?}"
        );
        let direct = persisted_coverage(&db, &consumer_path, "load().get");
        assert_eq!(direct.len(), 1, "{root}: {direct:?}");
        assert_eq!(direct[0].0, "external", "{root}: {direct:?}");
        let unknown = persisted_coverage(&db, &consumer_path, "node.unsupported_method");
        assert_eq!(unknown.len(), 1, "{root}: {unknown:?}");
        assert_eq!(unknown[0].0, "unresolved", "{root}: {unknown:?}");
        let fake = persisted_coverage(&db, &consumer_path, "fake.get");
        assert_eq!(fake.len(), 1, "{root}: {fake:?}");
        assert_eq!(fake[0].0, "unresolved", "{root}: {fake:?}");
    }
}
#[test]
fn collection_type_proof_matrix() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        CollectionLiteralsPropagateIntrinsicTypesThroughAliasesReturnsAndFields,
        ContainerAliasesPreserveArgumentsAndRequireBuiltinBaseProof,
    }
    struct SourceCase {
        kind: Case,
        path: &'static str,
        source: &'static str,
    }
    for row in [
        // collection_literals_propagate_intrinsic_types_through_aliases_returns_and_fields
        SourceCase { kind: Case::CollectionLiteralsPropagateIntrinsicTypesThroughAliasesReturnsAndFields, path: "literal_collections.py", source: "class dict:\n    def custom(self): pass\nclass list: pass\nclass set: pass\nclass tuple: pass\nraw: dict = {}\nalias = raw\nalias.get('key')\nitems: list[dict] = []\nitems.append(1)\nunique = {1}\nunique.add(2)\nordered = (1,)\nordered.count(1)\ncustom = dict()\ncustom.custom()\ndef make():\n    return {'key': 1}\ncreated = make()\ncreated.get('key')\nclass Store:\n    def __init__(self): self.data = {}\nstore = Store()\nstore.data.get('key')\nraw = unknown()\nraw.get('key')\n" },
        // container_aliases_preserve_arguments_and_require_builtin_base_proof
        SourceCase { kind: Case::ContainerAliasesPreserveArgumentsAndRequireBuiltinBaseProof, path: "containers.py", source: "from typing import Optional, Union\nclass Client:\n    def execute(self): pass\nclass Box:\n    def get(self): pass\nclass list:\n    def append(self, value): pass\ntype JsonValue = str | int | None\ntype JsonDict = dict[str, JsonValue]\ntype Maybe = Optional[Client]\ntype Either = Union[Client, str]\ntype Custom = Box[str]\ntype Shadowed = list[Client]\ndef use(data: JsonDict, maybe: Maybe, either: Either, custom: Custom, shadowed: Shadowed):\n    data.get('key')\n    maybe.execute()\n    either.execute()\n    custom.get()\n    shadowed.append(1)\n" },
    ] {
        let source = row.source;
        let facts = ast::extract(row.path, "python", source).unwrap();
        match row.kind {
            Case::CollectionLiteralsPropagateIntrinsicTypesThroughAliasesReturnsAndFields => {
let module = facts
        .nodes
        .iter()
        .find(|node| node.kind == acdd_index::models::tags::NodeKind::Module)
        .unwrap();
    let details = crate::common::assertions::projected_node_details(&facts, module);
    let flow: ValueFlowFacts = serde_json::from_value(details["value_flow"].clone()).unwrap();
    for (name, spelling) in [
        ("raw", "{}"),
        ("items", "[]"),
        ("unique", "{...}"),
        ("ordered", "()"),
    ] {
        assert_eq!(
            flow.bindings
                .iter()
                .find(|binding| binding.name == name)
                .unwrap()
                .value,
            ValueExpr::Construct {
                callee: spelling.to_owned()
            }
        );
    }
    let mut keys = Vec::new();
    flow.reference_keys(|key| keys.push(key));
    assert_eq!(keys, ["dict", "raw", "dict", "make", "Store", "unknown"]);
    let mut durable_keys = Vec::new();
    ValueFlowFacts::reference_keys_from_details(&details, |key| durable_keys.push(key));
    assert_eq!(durable_keys, keys);
            }
            Case::ContainerAliasesPreserveArgumentsAndRequireBuiltinBaseProof => {
let alias = facts
        .nodes
        .iter()
        .find(|node| node.name == "JsonDict")
        .unwrap();
    let details = crate::common::assertions::projected_node_details(&facts, alias);
    let flow: ValueFlowFacts = serde_json::from_value(details["value_flow"].clone()).unwrap();
    assert!(
        matches!(&flow.alias_type,Some(TypeExpr::Applied{base,args}) if base=="dict" && args.as_slice()==[TypeExpr::Named{name:"str".into()},TypeExpr::Named{name:"JsonValue".into()}])
    );
            }
        }
        let graph = linker::link(&BTreeMap::from([(row.path.to_owned(), facts)]));
        match row.kind {
            Case::CollectionLiteralsPropagateIntrinsicTypesThroughAliasesReturnsAndFields => {
for expression in [
        "alias.get",
        "items.append",
        "unique.add",
        "ordered.count",
        "created.get",
        "store.data.get",
    ] {
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == expression)
            .unwrap();
        assert_eq!(coverage.status, "external", "{expression}: {coverage:#?}");
        assert!(
            coverage.evidence.contains("Python standard library"),
            "{expression}: {coverage:#?}"
        );
    }
    assert!(graph
        .coverage
        .iter()
        .any(|coverage| coverage.expression == "custom.custom" && coverage.status == "resolved"));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.kind == "calls" && edge.evidence == "custom.custom"));
    assert_eq!(
        graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == "raw.get")
            .unwrap()
            .status,
        "unresolved"
    );
            }
            Case::ContainerAliasesPreserveArgumentsAndRequireBuiltinBaseProof => {
let data_get = graph
        .coverage
        .iter()
        .find(|coverage| coverage.expression == "data.get")
        .unwrap();
    assert_eq!(data_get.status, "external", "{data_get:#?}");
    assert!(data_get.evidence.contains("Python standard library"));
    for expression in [
        "maybe.execute",
        "either.execute",
        "custom.get",
        "shadowed.append",
    ] {
        assert_eq!(
            graph
                .coverage
                .iter()
                .find(|coverage| coverage.expression == expression)
                .unwrap()
                .status,
            "unresolved",
            "{expression}"
        );
    }
            }
        }
    }
}
