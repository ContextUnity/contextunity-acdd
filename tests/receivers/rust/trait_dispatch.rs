use super::*;

#[test]
fn visible_trait_dispatch_matrix() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        TraitMethodsRequireVisibleProvenTraits,
        TraitDefaultMethodsRequireAVisibleConcreteImpl,
    }
    for case in [
        Case::TraitMethodsRequireVisibleProvenTraits,
        Case::TraitDefaultMethodsRequireAVisibleConcreteImpl,
    ] {
        match case {
            // rust_trait_methods_require_visible_proven_traits
            Case::TraitMethodsRequireVisibleProvenTraits => {
                let facts: BTreeMap<_, _> = [
        ("src/model.rs", "pub struct Service;"),
        ("src/traits.rs", "pub trait Work { fn work(&self); }"),
        ("src/operations.rs", "use crate::model::Service; use crate::traits::Work;\nimpl Work for Service { fn work(&self) {} }"),
        ("src/client.rs", "use crate::model::Service; use crate::traits::Work;\nfn run(client: &Service) { client.work(); }"),
        ("src/no_trait.rs", "use crate::model::Service;\nfn run(client: &Service) { client.work(); }"),
        ("src/hidden_trait.rs", "use crate::model::Service;\nmod hidden { pub trait Hidden { fn hidden(&self); } }\nimpl hidden::Hidden for Service { fn hidden(&self) {} }\nfn run(client: &Service) { client.hidden(); }"),
    ].into_iter().map(|(path, source)| (path.to_owned(), ast::extract(path, "rust", source).unwrap())).collect();
                let graph = linker::link(&facts);
                assert!(
                    graph
                        .coverage
                        .iter()
                        .any(|coverage| coverage.path == "src/client.rs"
                            && coverage.expression == "client.work"
                            && coverage.status == "resolved"),
                    "{facts:#?}\n{graph:#?}"
                );
                assert!(graph
                    .coverage
                    .iter()
                    .any(|coverage| coverage.path == "src/no_trait.rs"
                        && coverage.expression == "client.work"
                        && coverage.status == "unresolved"));
                assert!(graph
                    .coverage
                    .iter()
                    .any(|coverage| coverage.path == "src/hidden_trait.rs"
                        && coverage.expression == "client.hidden"
                        && coverage.status == "unresolved"));
            }
            // rust_trait_default_methods_require_a_visible_concrete_impl
            Case::TraitDefaultMethodsRequireAVisibleConcreteImpl => {
                let facts: BTreeMap<_, _> = [
        ("src/model.rs", "pub struct Service;"),
        (
            "src/traits.rs",
            "pub trait Work { fn work(&self) {} fn overridden(&self) {} fn required(&self); }",
        ),
        (
            "src/operations.rs",
            "use crate::model::Service; use crate::traits::Work;\nimpl Work for Service { fn overridden(&self) {} fn required(&self) {} }",
        ),
        (
            "src/client.rs",
            "use crate::model::Service; use crate::traits::Work;\nfn run(client: &Service) { client.work(); client.overridden(); client.required(); }",
        ),
        (
            "src/no_trait.rs",
            "use crate::model::Service;\nfn run(client: &Service) { client.work(); }",
        ),
        (
            "src/no_impl.rs",
            "use crate::traits::Work;\nstruct Other;\nfn run(other: &Other) { other.work(); }",
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), ast::extract(path, "rust", source).unwrap()))
    .collect();
                let graph = linker::link(&facts);
                let default_method = facts["src/traits.rs"]
                    .nodes
                    .iter()
                    .find(|node| node.name == "work" && node.kind == acdd_index::models::tags::NodeKind::Method)
                    .unwrap();
                let implementations = &facts["src/operations.rs"].nodes;
                for (expression, target) in [
                    ("client.work", default_method),
                    (
                        "client.overridden",
                        implementations
                            .iter()
                            .find(|node| node.name == "overridden" && node.kind == acdd_index::models::tags::NodeKind::Method)
                            .unwrap(),
                    ),
                    (
                        "client.required",
                        implementations
                            .iter()
                            .find(|node| node.name == "required" && node.kind == acdd_index::models::tags::NodeKind::Method)
                            .unwrap(),
                    ),
                ] {
                    assert!(
                        graph.edges.iter().any(|edge| edge.path == "src/client.rs"
                            && edge.kind == "calls"
                            && edge.evidence == expression
                            && edge.dst == target.id),
                        "{graph:#?}"
                    );
                }
                assert!(graph
                    .coverage
                    .iter()
                    .any(|coverage| coverage.path == "src/no_trait.rs"
                        && coverage.expression == "client.work"
                        && coverage.status == "unresolved"));
                assert!(graph
                    .coverage
                    .iter()
                    .any(|coverage| coverage.path == "src/no_impl.rs"
                        && coverage.expression == "other.work"
                        && coverage.status == "unresolved"));
            }
        }
    }
}

#[test]
fn rust_computed_and_qualified_callees_keep_opaque_dispatch() {
    let source = "struct Service; trait Work { fn work(&self); }\nimpl Work for Service { fn work(&self) {} }\nfn choose() -> Service { Service {} }\nfn run(client: &Service) { choose().work(); <Service as Work>::work(client); }\nmacro_rules! invoke { () => {} }\nfn macro_call() { invoke!(); }";
    let facts = ast::extract("src/dispatch.rs", "rust", source).unwrap();
    assert!(facts.errors.is_empty(), "{facts:#?}");
    for expression in ["choose().work", "<Service as Work>::work"] {
        assert!(
            facts
                .references
                .iter()
                .any(|reference| reference.kind == "calls"
                    && reference.expression == expression
                    && reference.dynamic),
            "{facts:#?}"
        );
    }
    assert!(
        facts
            .references
            .iter()
            .any(|reference| reference.kind == "calls"
                && reference.expression == "invoke"
                && !reference.dynamic),
        "{facts:#?}"
    );
    let graph = linker::link(&BTreeMap::from([("src/dispatch.rs".to_owned(), facts)]));
    for expression in ["choose().work", "<Service as Work>::work"] {
        assert!(
            graph
                .coverage
                .iter()
                .any(|coverage| coverage.expression == expression
                    && coverage.status == "unresolved"
                    && coverage.evidence.starts_with("computed receiver")),
            "{graph:#?}"
        );
    }
    assert!(
        graph
            .coverage
            .iter()
            .any(|coverage| coverage.expression == "choose" && coverage.status == "resolved"),
        "{graph:#?}"
    );
}

#[test]
fn wildcard_generic_default_and_macro_trait_delegation_resolve() {
    let facts: BTreeMap<_, _> = [
        (
            "src/traits.rs",
            "pub trait Work { fn work(&self); }\npub trait Borrowed<'a> { fn borrow_work(&self); }\n#[macro_export]\nmacro_rules! delegate_work {\n    ($ty:ty) => {\n        impl $crate::traits::Work for $ty { fn work(&self) {} }\n    };\n}\n",
        ),
        (
            "src/private_macros.rs",
            "pub trait PrivTrait { fn priv_work(&self); }\nmacro_rules! secret_delegate { ($ty:ty) => { impl $crate::private_macros::PrivTrait for $ty { fn priv_work(&self) {} } }; }\n",
        ),
        (
            "src/dup_a.rs",
            "pub trait DupA { fn dup_work(&self); }\n#[macro_export]\nmacro_rules! dup_macro { ($ty:ty) => { impl $crate::dup_a::DupA for $ty { fn dup_work(&self) {} } }; }\n",
        ),
        (
            "src/dup_b.rs",
            "pub trait DupB { fn dup_work(&self); }\n#[macro_export]\nmacro_rules! dup_macro { ($ty:ty) => { impl $crate::dup_b::DupB for $ty { fn dup_work(&self) {} } }; }\n",
        ),
        (
            "src/model.rs",
            "use super::traits::*;\nuse super::private_macros::PrivTrait;\nuse super::dup_a::*;\nuse super::dup_b::*;\npub struct Service;\nimpl Default for Service { fn default() -> Self { Self } }\nimpl Work for Service { fn work(&self) {} }\nimpl<'a> Borrowed<'a> for Service { fn borrow_work(&self) {} }\npub struct Delegated;\nimpl Delegated { pub fn new() -> Self { Self } }\ncrate::traits::delegate_work!(Delegated);\nsecret_delegate!(Delegated);\ndup_macro!(Delegated);\nfn run_wild(svc: &Service) { svc.work(); svc.borrow_work(); let def = Service::default(); def.work(); }\n#[cfg(test)]\nmod tests {\n    use crate::traits::Work;\n    use crate::private_macros::PrivTrait;\n    use crate::dup_a::DupA;\n    use crate::model::Delegated;\n    fn check() { Delegated::new().work(); Delegated::new().priv_work(); Delegated::new().dup_work(); }\n}\n",
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), ast::extract(path, "rust", source).unwrap()))
    .collect();
    let graph = linker::link(&facts);
    for (expr, expected) in [
        ("svc.work", "resolved"),
        ("svc.borrow_work", "resolved"),
        ("Service::default", "resolved"),
        ("def.work", "resolved"),
        ("Delegated::new().work", "resolved"),
        ("Delegated::new().priv_work", "unresolved"),
        ("Delegated::new().dup_work", "unresolved"),
    ] {
        assert!(
            graph
                .coverage
                .iter()
                .any(|c| c.path == "src/model.rs" && c.expression == expr && c.status == expected),
            "expected {expected} for {expr}: {graph:#?}"
        );
    }
}
