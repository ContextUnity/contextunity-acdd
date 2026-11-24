use super::*;
use acdd_index::languages::manifests::{ClassHierarchyReceiverRule, ReturnPolicy};
use acdd_index::linker::value_flow::{
    ConfiguredReceiverAuthority, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};
use acdd_index::{
    models::{Facts, Node},
    semantic::{SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts},
};

struct SeamValueFlowResolver;

impl<'a> acdd_index::linker::value_flow::SemanticResolver<'a> for SeamValueFlowResolver {
    fn resolve_symbol(
        &self,
        _scope: &'a acdd_index::models::Node,
        _name: &str,
        _at: acdd_index::semantic::SourcePosition,
        _role: acdd_index::linker::value_flow::SymbolRole,
    ) -> acdd_index::linker::value_flow::Symbol<'a> {
        acdd_index::linker::value_flow::Symbol::Unknown
    }
}

const SOURCE: &str = r#"
use std::collections::HashMap;

pub struct Config {
    pub host: String,
    pub port: u16,
}

pub enum Status {
    Active,
    Inactive,
}

impl Config {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }
}
"#;

fn facts() -> Facts {
    ExtractorHarness::for_lang("rust")
        .at_path("test.rs")
        .extract(SOURCE)
}

fn declaration<'a>(facts: &'a Facts, kind: &str, name: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap()
}

fn call(callee: &str) -> ValueExpr {
    ValueExpr::Call {
        callee: callee.to_owned(),
        site: None,
    }
}

fn supplied_projection(member: &str) -> ValueExpr {
    ValueExpr::Project {
        value: Box::new(ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: "Supplied".to_owned(),
            },
        }),
        member: member.to_owned(),
    }
}

fn after(owner: &Node) -> SourcePosition {
    SourcePosition {
        line: owner.line + 1,
        column: 0,
    }
}

struct DotConstructorResolver<'a> {
    class: &'a Node,
    constructor: &'a Node,
    flow: &'a ValueFlowFacts,
}
impl<'a> SemanticResolver<'a> for DotConstructorResolver<'a> {
    fn resolve_symbol(
        &self,
        _: &'a Node,
        name: &str,
        _: SourcePosition,
        role: SymbolRole,
    ) -> Symbol<'a> {
        match (name, role) {
            ("Config", SymbolRole::Type) => Symbol::Type(TypeTarget::Local(self.class)),
            _ => Symbol::Unknown,
        }
    }
    fn resolve_member(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        _: &'a Node,
        _: SourcePosition,
    ) -> Symbol<'a> {
        if matches!(receiver, TypeTarget::Local(node) if node.id == self.class.id)
            && member == "new"
        {
            Symbol::Callable(self.constructor)
        } else {
            Symbol::Unknown
        }
    }
    fn value_flow(&self, node: &'a Node) -> Option<&'a ValueFlowFacts> {
        (node.id == self.constructor.id).then_some(self.flow)
    }
}

#[test]
fn constructor_and_hint_free_flow() {
    let facts = facts();
    let empty_catalog = std::collections::BTreeMap::new();
    let seam_resolver = SeamValueFlowResolver;
    let _seam_index =
        ValueFlowIndex::build_with_limits(&empty_catalog, &seam_resolver, Default::default());
    let class = declaration(&facts, "struct", "Config");
    let constructor = declaration(&facts, "method", "new");
    let flow = ValueFlowFacts {
        return_type: Some(TypeExpr::Named {
            name: "Self".to_owned(),
        }),
        ..Default::default()
    };
    let resolver = DotConstructorResolver {
        class,
        constructor,
        flow: &flow,
    };
    let index = ValueFlowIndex::build(&empty_catalog, &resolver);

    let source = r#"
struct LeafBatch9 {}
impl LeafBatch9 { fn finish(&self) {} }
struct HubBatch9 {}
impl HubBatch9 { fn leaf(&self) -> LeafBatch9 { LeafBatch9 {} } }
fn probe(name: &str, path: &std::path::PathBuf, hub: HubBatch9, items: Vec<LeafBatch9>) {
    name.trim().is_empty();
    path.parent().unwrap().exists();
    hub.leaf().finish();
    hub.missing().finish();
    hub.leaf(unknown).finish();
    for mut item in items { item.finish(); }
    let mut bound = LeafBatch9 {}; bound.finish();
}
"#;
    let mut extracted = ExtractorHarness::for_lang("rust")
        .at_path("batch9.rs")
        .extract(source);
    for reference in &mut extracted.references {
        reference.receiver_hint = None;
    }
    let graph = ExtractorHarness::link_graph(&std::collections::BTreeMap::from([(
        "batch9.rs".into(),
        extracted,
    )]));

    enum Input {
        Constructor(&'static str),
        Receiver(&'static str),
    }
    let mut cases = vec![
        (
            "constructor_spellings_normalize_and_missing_constructors_stay_unknown",
            Input::Constructor("Config::new"),
            true,
        ),
        (
            "constructor_spellings_normalize_and_missing_constructors_stay_unknown",
            Input::Constructor("Config.new"),
            true,
        ),
        (
            "constructor_spellings_normalize_and_missing_constructors_stay_unknown",
            Input::Constructor("Config::create"),
            false,
        ),
    ];
    for (expression, expected) in [
        ("name.trim().is_empty", true),
        ("path.parent().unwrap().exists", true),
        ("hub.leaf().finish", true),
        ("hub.missing().finish", false),
        ("hub.leaf(unknown).finish", false),
        ("item.finish", true),
        ("bound.finish", true),
    ] {
        cases.push((
            "hint_free_value_flow_resolves_only_proven_receiver_chains",
            Input::Receiver(expression),
            expected,
        ));
    }
    for (case_id, input, expected) in cases {
        let (expression, observed) = match input {
            Input::Constructor(callee) => {
                let result = index.resolve_value(
                    class,
                    &call(callee),
                    SourcePosition {
                        line: 20,
                        column: 0,
                    },
                    &resolver,
                );
                if !expected {
                    assert!(
                        matches!(result, TypeTarget::Unknown),
                        "{case_id}: {callee}: {result:?}"
                    );
                }
                (
                    callee,
                    matches!(result, TypeTarget::Local(node) if node.id == class.id),
                )
            }
            Input::Receiver(expression) => {
                let row = graph
                    .coverage
                    .iter()
                    .find(|row| row.expression == expression)
                    .unwrap();
                (
                    expression,
                    matches!(row.status.as_str(), "resolved" | "external"),
                )
            }
        };
        assert_eq!(observed, expected, "{case_id}: {expression}");
    }
}

fn configured(base: &str, member: &str, returns: bool) -> TypeTarget<'static> {
    TypeTarget::ConfiguredReceiver {
        authority: std::sync::Arc::new(ConfiguredReceiverAuthority::Hierarchy {
            pack: "supplied-rust-policy".to_owned(),
            rule: std::sync::Arc::new(ClassHierarchyReceiverRule {
                bases: vec![base.to_owned()],
                provider: "supplied".to_owned(),
                members: vec![member.to_owned()],
                returns: if returns {
                    std::collections::BTreeMap::from([(
                        member.to_owned(),
                        ReturnPolicy::Receiver {
                            base: "Replacement".to_owned(),
                        },
                    )])
                } else {
                    Default::default()
                },
            }),
        }),
        origin: Box::new(TypeTarget::External {
            module: base.to_owned(),
            import_line: 1,
        }),
        fallback: None,
        evidence: "supplied-policy".to_owned(),
    }
}
struct SuppliedResolver {
    receiver: TypeTarget<'static>,
    result: Option<TypeTarget<'static>>,
}
impl<'a> SemanticResolver<'a> for SuppliedResolver {
    fn resolve_symbol(
        &self,
        _: &'a Node,
        name: &str,
        _: SourcePosition,
        _: SymbolRole,
    ) -> Symbol<'a> {
        if name == "Supplied" {
            Symbol::Type(self.receiver.clone())
        } else {
            Symbol::Unknown
        }
    }
    fn resolve_member_result(
        &self,
        receiver: &TypeTarget<'a>,
        member: &str,
        _: &'a Node,
        _: SourcePosition,
    ) -> Option<TypeTarget<'a>> {
        let TypeTarget::ConfiguredReceiver { authority, .. } = receiver else {
            return None;
        };
        let ConfiguredReceiverAuthority::Hierarchy { rule, .. } = authority.as_ref() else {
            return None;
        };
        rule.returns.get(member)?;
        self.result.clone()
    }
}

#[test]
fn configured_return_authority() {
    let facts = facts();
    let empty_catalog = std::collections::BTreeMap::new();
    let owner = declaration(&facts, "method", "new");
    let at = after(owner);
    let mut cases = Vec::new();
    for (base, member, wrappers) in [
        ("rusqlite::Rows", "next", 2),
        ("rusqlite::Row", "get", 1),
        ("std::process::Command", "output", 1),
        ("walkdir::WalkDir", "into_iter", 1),
    ] {
        for returns in [true, false] {
            cases.push((
                "supplied_return_authority_controls_nested_wrapper_projections",
                base,
                member,
                wrappers,
                returns,
                false,
            ));
        }
    }
    cases.push((
        "nested_configured_results_without_return_authority_remain_unknown",
        "rusqlite::Row",
        "get",
        1,
        true,
        true,
    ));
    for (case_id, base, member, wrappers, returns, nested_unknown) in cases {
        let resolver = SuppliedResolver {
            receiver: configured(base, member, returns),
            result: Some(if nested_unknown {
                configured("std::result::Result", "unwrap", false)
            } else {
                TypeTarget::Builtin("Replacement".to_owned())
            }),
        };
        let index = ValueFlowIndex::build(&empty_catalog, &resolver);
        let mut result = index.resolve_value(owner, &supplied_projection(member), at, &resolver);
        if !returns {
            assert!(
                matches!(result, TypeTarget::Unknown),
                "{case_id}: {base}.{member}: {result:?}"
            );
            continue;
        }
        if member == "into_iter" {
            let TypeTarget::RustIterator { item } = result else {
                panic!("{case_id}: {base}.{member}: {result:?}");
            };
            result = *item;
        }
        for depth in 0..wrappers {
            let TypeTarget::RustWrapper { wrapper, ok, .. } = result else {
                panic!("{case_id}: {base}.{member}: {result:?}");
            };
            assert_eq!(
                wrapper,
                if depth == 1 {
                    acdd_index::linker::value_flow::RustWrapperKind::Option
                } else {
                    acdd_index::linker::value_flow::RustWrapperKind::Result
                }
            );
            result = *ok;
        }
        if nested_unknown {
            assert!(
                matches!(result, TypeTarget::Unknown),
                "{case_id}: {result:?}"
            );
        } else {
            assert!(
                matches!(result, TypeTarget::Builtin(name) if name == "Replacement"),
                "{case_id}: {base}.{member}"
            );
        }
    }
}

#[test]
fn declarations_and_static_constructor_metadata_are_preserved() {
    let facts = facts();
    assert!(facts
        .nodes
        .iter()
        .any(|n| n.kind == "struct" && n.name == "Config"));
    assert!(facts
        .nodes
        .iter()
        .any(|n| n.kind == "enum" && n.name == "Status"));
    assert!(facts
        .nodes
        .iter()
        .any(|n| n.kind == "method" && n.name == "new"));
    let constructor = facts
        .nodes
        .iter()
        .find(|n| n.name == "new")
        .expect("new method missing");
    assert_eq!(constructor.details["receiver_type"], "Config");
    assert!(constructor.details.get("is_method").is_none());
    assert_eq!(constructor.details["is_static"], true);
    let profile = acdd_index::languages::require("rust").expect("rust profile missing");
    assert_eq!(profile.symbol_kind("function_item"), Some("function"));
    let _ = profile.grammar("lib.rs");
}

#[test]
fn enum_variant_attributes_skipped_and_serde_callbacks_linked() {
    let source = r##"
pub struct Payload;
pub struct Codec;
impl Codec {
    pub fn is_empty_codec(&self) -> bool { false }
    pub fn default_flag() -> bool { true }
}
fn yes() -> bool { true }
fn skip_item(_v: &Option<Payload>) -> bool { false }
fn ser_fn<S>(_v: &Payload, _s: S) -> Result<(), ()> { Ok(()) }
fn de_fn<'de, D>(_d: D) -> Result<Payload, ()> { Err(()) }
mod custom_codec {
    pub fn serialize() {}
    pub fn deserialize() {}
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default = "Codec::default_flag")]
pub enum Command {
    /// Struct variant with clap & serde attributes and comments.
    Run {
        #[arg(long, default_value_t = false)]
        /// Comment inside variant body.
        all: bool,
        /* block comment */
        #[serde(default = "yes", skip_serializing_if = "skip_item")]
        payload: Option<Payload>,
        #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    Tuple(#[serde(serialize_with = "ser_fn", deserialize_with = r#"de_fn"#)] Payload),
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct Record {
    #[serde(default)]
    bare_default: bool,
    #[serde(with = "custom_codec")]
    wrapped: Payload,
}
"##;
    let facts = ExtractorHarness::for_lang("rust")
        .at_path("src/lib.rs")
        .extract(source);
    for bogus in ["arg", "long", "default_value_t", "serde", "kind"] {
        assert!(
            !facts.references.iter().any(|r| r.expression == bogus),
            "attribute token `{bogus}` leaked into references: {:#?}",
            facts.references
        );
    }
    for expected_ref in [
        "Payload",
        "Codec::default_flag",
        "yes",
        "skip_item",
        "Option::is_none",
        "ser_fn",
        "de_fn",
        "custom_codec",
    ] {
        assert!(
            facts.references.iter().any(|r| r.expression == expected_ref),
            "missing expected reference `{expected_ref}`: {:#?}",
            facts.references
        );
    }

    let graph = ExtractorHarness::link_graph(&std::collections::BTreeMap::from([(
        "src/lib.rs".to_owned(),
        facts,
    )]));
    for (expr, expected_status) in [
        ("Codec::default_flag", "resolved"),
        ("yes", "resolved"),
        ("skip_item", "resolved"),
        ("ser_fn", "resolved"),
        ("de_fn", "resolved"),
        ("Option::is_none", "external"),
        ("Payload", "resolved"),
    ] {
        let matched = graph
            .coverage
            .iter()
            .filter(|c| c.expression == expr)
            .collect::<Vec<_>>();
        assert!(
            matched.iter().any(|c| c.status == expected_status),
            "`{expr}` expected status `{expected_status}`, got {matched:#?}"
        );
    }
}
