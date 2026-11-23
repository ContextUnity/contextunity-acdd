/// Manifest activation conditions and language parsing.
pub mod activation;
/// Typed framework rule data loaded from `.acdd/frameworks/*.toml`, `.yaml`, or `.yml`.
pub mod framework;

pub use framework::{
    ClassHierarchyReceiverRule, DiCallRule, FrameworkManifest, ReturnPolicy, RouteCallRule,
    RouteDecoratorRule, RuleCoverageCode, RuleCoverageDiagnostic, RuleDomain, RuleIdentity,
};

/// Manifest projections support.
pub mod projections;
/// Manifest semantic rule engine.
pub mod semantic;
/// Manifest syntax operations and AST rules.
pub mod syntax;
pub use semantic::{ActivationCondition, ActivationMatch};
pub(crate) use semantic::{SemanticCallee, SemanticOperation, SemanticRule, SemanticTarget};

/// Resolves a visible named kind without treating an unknown name as kind zero.
pub(crate) fn named_kind(grammar: &tree_sitter::Language, name: &str) -> Option<u16> {
    let kind = grammar.id_for_node_kind(name, true);
    (grammar.node_kind_for_id(kind) == Some(name) && grammar.node_kind_is_named(kind))
        .then_some(kind)
}
