use acdd_index::linker::value_flow::types::ValueFlowLookup;
use acdd_index::linker::value_flow::{
    SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};
use acdd_index::{
    models::{Facts, Node},
    semantic::{BindingFact, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts},
};
use std::collections::BTreeMap;

struct BuiltinResolver;

impl<'a> SemanticResolver<'a> for BuiltinResolver {
    fn resolve_symbol(
        &self,
        _scope: &'a Node,
        name: &str,
        _at: SourcePosition,
        _role: SymbolRole,
    ) -> Symbol<'a> {
        Symbol::Type(TypeTarget::Builtin(name.into()))
    }
}

#[test]
fn replay_preserves_duplicate_positions_and_strict_source_order() {
    let position = |line, column| SourcePosition { line, column };
    let binding = |name: &str, line, column, type_name: &str| BindingFact {
        name: name.into(),
        position: position(line, column),
        value: ValueExpr::Annotated {
            type_expr: TypeExpr::Named {
                name: type_name.into(),
            },
        },
        conditional: false,
    };
    let flow = ValueFlowFacts {
        bindings: vec![
            binding("value", 2, 0, "str"),
            binding("value", 2, 0, "int"),
            binding("value", 4, 0, "str"),
            binding("value", 6, 0, "int"),
            binding("ordered", 7, 8, "str"),
            BindingFact {
                name: "ordered".into(),
                position: position(7, 20),
                value: ValueExpr::Unknown,
                conditional: false,
            },
        ],
        ..Default::default()
    };
    let owner = Node {
        native_semantic: None,
        id: "owner".into(),
        kind: acdd_index::models::tags::NodeKind::Function,
        name: "owner".into(),
        qualname: "owner".into(),
        path: "test.py".into(),
        line: 1,
        end_line: 8,
        is_test: true,
        language: acdd_index::models::tags::LanguageId::Python,
        generated: false,
        details: serde_json::json!({"value_flow": flow}),
    };
    let mut all = BTreeMap::new();
    all.insert(
        owner.path.clone(),
        Facts {
            nodes: vec![owner],
            ..Default::default()
        },
    );
    let owner = &all["test.py"].nodes[0];
    let index = ValueFlowIndex::build(&all, &BuiltinResolver);
    assert!(matches!(
        index.lookup(owner, "value", position(3, 0)),
        Some(TypeTarget::Unknown)
    ));
    assert!(
        matches!(index.lookup(owner, "value", position(5, 0)), Some(TypeTarget::Builtin(name)) if name == "str")
    );
    assert!(
        matches!(index.lookup(owner, "value", position(7, 0)), Some(TypeTarget::Builtin(name)) if name == "int")
    );
    assert!(matches!(
        index.lookup(owner, "ordered", position(7, 8)),
        Some(TypeTarget::Unknown)
    ));
    assert!(matches!(
        index.lookup(owner, "ordered", position(7, 19)),
        Some(TypeTarget::Builtin(name)) if name == "str"
    ));
    assert!(matches!(
        index.lookup(owner, "ordered", position(7, 21)),
        Some(TypeTarget::Unknown)
    ));
}

#[test]
fn semantic_limits_reject_excessive_or_invalid_facts() {
    let facts = ValueFlowFacts {
        bindings: vec![BindingFact {
            name: "value".into(),
            position: SourcePosition { line: 1, column: 0 },
            value: ValueExpr::Annotated {
                type_expr: TypeExpr::Named { name: "str".into() },
            },
            conditional: false,
        }],
        ..ValueFlowFacts::default()
    };
    let owner = Node {
        native_semantic: None,
        id: "owner".into(),
        kind: acdd_index::models::tags::NodeKind::Function,
        name: "owner".into(),
        qualname: "owner".into(),
        path: "limits.py".into(),
        line: 1,
        end_line: 3,
        is_test: true,
        language: acdd_index::models::tags::LanguageId::Python,
        generated: false,
        details: serde_json::json!({"value_flow": facts}),
    };
    let all = BTreeMap::from([(
        owner.path.clone(),
        Facts {
            nodes: vec![owner],
            ..Default::default()
        },
    )]);
    let owner = &all["limits.py"].nodes[0];
    let default_index = ValueFlowIndex::build(&all, &BuiltinResolver);
    assert!(matches!(
        default_index.lookup(owner, "value", SourcePosition { line: 2, column: 0 }),
        Some(TypeTarget::Builtin(name)) if name == "str"
    ));

    let bounded_index = ValueFlowIndex::build_with_limits(
        &all,
        &BuiltinResolver,
        SemanticLimits {
            facts_per_scope: 0,
            ..SemanticLimits::default()
        },
    );
    assert!(!matches!(
        bounded_index.lookup(owner, "value", SourcePosition { line: 2, column: 0 }),
        Some(TypeTarget::Builtin(_))
    ));

    let mut invalid = all.clone();
    let owner = &mut invalid.get_mut("limits.py").unwrap().nodes[0];
    let mut facts: ValueFlowFacts =
        serde_json::from_value(owner.details["value_flow"].clone()).unwrap();
    facts.bindings[0].position.line = 0;
    owner.details["value_flow"] = serde_json::to_value(facts).unwrap();
    let invalid_index = ValueFlowIndex::build(&invalid, &BuiltinResolver);
    let owner = &invalid["limits.py"].nodes[0];
    assert!(!matches!(
        invalid_index.lookup(owner, "value", SourcePosition { line: 2, column: 0 }),
        Some(TypeTarget::Builtin(_))
    ));
}

#[test]
fn named_type_aliases_resolve_with_cycle_and_depth_guards() {
    fn alias(name: &str, rhs: &str) -> Node {
        Node {
            native_semantic: None,
            id: name.into(),
            kind: acdd_index::models::tags::NodeKind::Type,
            name: name.into(),
            qualname: name.into(),
            path: "test.py".into(),
            line: 1,
            end_line: 1,
            is_test: true,
            language: acdd_index::models::tags::LanguageId::Python,
            generated: false,
            details: serde_json::json!({"value_flow": {"alias_type": {"kind": "named", "name": rhs}}}),
        }
    }
    struct Resolver<'a> {
        nodes: Vec<&'a Node>,
    }
    impl<'a> SemanticResolver<'a> for Resolver<'a> {
        fn resolve_symbol(
            &self,
            _scope: &'a Node,
            name: &str,
            _at: SourcePosition,
            _role: SymbolRole,
        ) -> Symbol<'a> {
            if name == "str" {
                return Symbol::Type(TypeTarget::Builtin("str".into()));
            }
            self.nodes
                .iter()
                .find(|node| node.name == name)
                .map_or(Symbol::Unknown, |node| {
                    Symbol::Type(TypeTarget::Local(node))
                })
        }
    }
    let all = BTreeMap::from([(
        "test.py".to_owned(),
        Facts {
            nodes: vec![
                alias("First", "Second"),
                alias("Second", "str"),
                alias("Loop", "Loop"),
            ],
            ..Default::default()
        },
    )]);
    let nodes = &all["test.py"].nodes;
    let first = &nodes[0];
    let resolver = Resolver {
        nodes: nodes.iter().collect(),
    };
    let at = SourcePosition { line: 3, column: 0 };
    let named = |name: &str| TypeExpr::Named { name: name.into() };
    let resolve = |expression, limits| {
        let index = ValueFlowIndex::build_with_limits(&all, &resolver, limits);
        index.resolve_value(
            first,
            &ValueExpr::Annotated {
                type_expr: expression,
            },
            at,
            &resolver,
        )
    };
    assert!(matches!(
        resolve(named("First"), SemanticLimits::default()),
        TypeTarget::Builtin(name) if name == "str"
    ));
    assert!(matches!(
        resolve(named("Loop"), SemanticLimits::default()),
        TypeTarget::Unknown
    ));
    assert!(matches!(
        resolve(
            named("First"),
            SemanticLimits {
                alias_depth: 1,
                ..SemanticLimits::default()
            }
        ),
        TypeTarget::Unknown
    ));
    let wrapper = TypeExpr::Applied {
        base: "Optional".into(),
        args: vec![named("str")],
    };
    assert!(matches!(
        resolve(wrapper, SemanticLimits::default()),
        TypeTarget::Unknown
    ));
}

#[test]
fn inferred_factory_returns_relink_bindings_and_leave_cycles_unknown() {
    fn node(name: &str, kind: &str, line: usize, flow: ValueFlowFacts) -> Node {
        Node {
            native_semantic: None,
            id: name.into(),
            kind: acdd_index::models::tags::NodeKind::parse(kind).expect("valid node kind"),
            name: name.into(),
            qualname: name.into(),
            path: "test.py".into(),
            line,
            end_line: line + 1,
            is_test: true,
            language: acdd_index::models::tags::LanguageId::Python,
            generated: false,
            details: serde_json::json!({"value_flow": flow}),
        }
    }
    struct Resolver<'a>(&'a [Node]);
    impl<'a> SemanticResolver<'a> for Resolver<'a> {
        fn resolve_symbol(
            &self,
            _scope: &'a Node,
            name: &str,
            _at: SourcePosition,
            role: SymbolRole,
        ) -> Symbol<'a> {
            let Some(node) = self.0.iter().find(|node| node.name == name) else {
                return Symbol::Unknown;
            };
            match role {
                SymbolRole::Type | SymbolRole::Constructor
                    if node.kind == acdd_index::models::tags::NodeKind::Class =>
                {
                    Symbol::Type(TypeTarget::Local(node))
                }
                SymbolRole::Callable
                    if node.kind == acdd_index::models::tags::NodeKind::Function =>
                {
                    Symbol::Callable(node)
                }
                _ => Symbol::Unknown,
            }
        }
    }
    let position = |line| SourcePosition { line, column: 0 };
    #[allow(clippy::field_reassign_with_default)]
    let returning = |name: &str, line| {
        let mut framework = acdd_index::semantic::FrameworkScopeFacts::default();
        framework.return_value = Some(ValueExpr::Call {
            callee: name.into(),
            site: None,
        });
        framework.return_position = Some(position(line));
        ValueFlowFacts {
            framework: Some(Box::new(framework)),
            ..Default::default()
        }
    };
    let nodes = vec![
        node("wrapper", "function", 1, returning("maker", 2)),
        node("maker", "function", 3, returning("Worker", 4)),
        node("Worker", "class", 5, ValueFlowFacts::default()),
        node("cycle_a", "function", 7, returning("cycle_b", 8)),
        node("cycle_b", "function", 9, returning("cycle_a", 10)),
        node(
            "use",
            "function",
            11,
            ValueFlowFacts {
                bindings: vec![BindingFact {
                    name: "client".into(),
                    position: position(12),
                    value: ValueExpr::Call {
                        callee: "wrapper".into(),
                        site: None,
                    },
                    conditional: false,
                }],
                ..Default::default()
            },
        ),
        node(
            "explicit_unknown",
            "function",
            14,
            ValueFlowFacts {
                return_type: Some(TypeExpr::Unknown),
                ..returning("Worker", 15)
            },
        ),
    ];
    let mut all = BTreeMap::new();
    all.insert(
        "test.py".into(),
        Facts {
            nodes,
            ..Default::default()
        },
    );
    let nodes = &all["test.py"].nodes;
    let index = ValueFlowIndex::build(&all, &Resolver(nodes));
    assert!(
        matches!(index.return_type(&nodes[0]), Some(TypeTarget::Local(node)) if node.name == "Worker")
    );
    assert!(
        matches!(index.lookup(&nodes[5], "client", position(13)), Some(TypeTarget::Local(node)) if node.name == "Worker")
    );
    assert!(matches!(
        index.return_type(&nodes[3]),
        Some(TypeTarget::Unknown)
    ));
    assert!(matches!(
        index.return_type(&nodes[4]),
        Some(TypeTarget::Unknown)
    ));
    assert!(matches!(
        index.return_type(&nodes[6]),
        Some(TypeTarget::Unknown)
    ));
}

#[test]
fn ordinary_typescript_class_call_is_not_a_constructor() {
    struct Resolver<'a>(&'a Node);
    impl<'a> SemanticResolver<'a> for Resolver<'a> {
        fn resolve_symbol(
            &self,
            _scope: &'a Node,
            _name: &str,
            _at: SourcePosition,
            _role: SymbolRole,
        ) -> Symbol<'a> {
            Symbol::Type(TypeTarget::Local(self.0))
        }
    }
    let class = Node {
        native_semantic: None,
        id: "Worker".into(),
        kind: acdd_index::models::tags::NodeKind::Class,
        name: "Worker".into(),
        qualname: "Worker".into(),
        path: "test.ts".into(),
        line: 1,
        end_line: 2,
        is_test: true,
        language: acdd_index::models::tags::LanguageId::Typescript,
        generated: false,
        details: serde_json::Value::Null,
    };
    let mut all = BTreeMap::new();
    all.insert(
        class.path.clone(),
        Facts {
            nodes: vec![class],
            ..Default::default()
        },
    );
    let class = &all["test.ts"].nodes[0];
    let resolver = Resolver(class);
    let index = ValueFlowIndex::build(&all, &resolver);
    let at = SourcePosition { line: 3, column: 0 };
    assert!(index
        .type_of_expression_tracked(class, "Worker", at, &resolver, None)
        .is_none());
    assert!(matches!(
        index.resolve_value(
            class,
            &ValueExpr::Call {
                callee: "Worker".into(),
                site: None,
            },
            at,
            &resolver
        ),
        TypeTarget::Unknown
    ));
    assert!(
        matches!(index.resolve_value(class, &ValueExpr::Construct { callee: "Worker".into() }, at, &resolver), TypeTarget::Local(node) if node.id == "Worker")
    );
}

#[test]
#[cfg(all(feature = "lang-rust", feature = "lang-python", feature = "lang-typescript"))]
fn local_binding_mutates_stay_distinct_from_shadowed_calls_across_languages() {
    use acdd_index::{ast, models::Reference};

    const SHADOWED_CALLEE: &str =
        "callee is shadowed by a parameter or local binding of unknown callable identity";
    let cases = [
        (
            "lib.rs",
            "rust",
            "struct Counter { count: i32 }\nimpl Counter { fn inc(&mut self) { self.count += 1; } }\nfn run() {}\nfn driver(run: usize, mut local: i32) { local += 1; run(); }\n",
            "self.count",
            None,
        ),
        (
            "service.py",
            "python",
            "class Counter:\n    count = 0\n    def inc(self): self.count += 1\ndef run(): pass\ndef driver(run, local):\n    local = 1\n    run()\ndef free_mut(self): self.count = 2\n",
            "self.count",
            Some(("free_mut", "self.count")),
        ),
        (
            "service.ts",
            "typescript",
            "class Counter { count = 0; inc() { this.count += 1; } }\nfunction run() {}\nfunction driver(run: unknown, local: number) { local = 1; run(); }\nfunction free_mut() { const self = { count: 0 }; self.count = 2; }\n",
            "this.count",
            Some(("free_mut", "self.count")),
        ),
    ];

    for (path, lang, source, real_field_expr, extracted_local_member) in cases {
        let mut facts = ast::extract(path, lang, source).unwrap();
        assert!(facts.errors.is_empty(), "{lang}: {:?}", facts.errors);
        let driver = facts.nodes.iter().find(|n| n.name == "driver").unwrap().clone();
        facts.references.push(Reference {
            source: driver.id.clone(),
            expression: "local".into(),
            kind: "mutates".into(),
            line: driver.line,
            column: 4,
            dynamic: false,
            alias: None,
            module: None,
            receiver_hint: None,
        });
        let all = BTreeMap::from([(path.to_owned(), facts)]);
        let graph = crate::common::link_facts(&all);

        assert!(
            graph.edges.iter().any(|e| {
                e.kind == "mutates"
                    && e.evidence == real_field_expr
                    && all[path].nodes.iter().any(|n| n.id == e.dst && n.name == "count")
            }),
            "{lang}: missing real field mutates edge: {:#?}",
            graph.edges
        );
        assert!(
            graph.coverage.iter().any(|r| {
                r.expression == "run" && r.status == "unresolved" && r.evidence == SHADOWED_CALLEE
            }) && !graph
                .coverage
                .iter()
                .any(|r| r.expression == "local" && r.status == "unresolved"),
            "{lang}: shadowed call vs local Mutates coverage mismatch: {:#?}",
            graph.coverage
        );
        if let Some((owner_name, expr)) = extracted_local_member {
            let line = all[path].nodes.iter().find(|n| n.name == owner_name).unwrap().line;
            assert!(
                !graph
                    .coverage
                    .iter()
                    .any(|r| r.expression == expr && r.line >= line && r.status == "unresolved"),
                "{lang}: extracted local {expr} Mutates unresolved: {:#?}",
                graph.coverage
            );
        }
    }
}
