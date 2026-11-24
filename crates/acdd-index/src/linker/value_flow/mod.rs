//! Value-flow evaluation and type target resolution.

mod evaluate;
pub(in crate::linker) use evaluate::collection_callback;
mod initialize;
mod lookup;
pub mod types;

pub(in crate::linker) use lookup::resolve_type;
pub(crate) use lookup::stored_type_expr;
pub(in crate::linker) use types::node_position;
pub(crate) use types::{valid_scope_facts, Binding, MAX_CANDIDATE_BRANCH_JOINS};
pub use types::{
    ConfiguredCallResult, ConfiguredNominalReceiver, ConfiguredReceiverAuthority, RustWrapperKind,
    SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};

#[cfg(test)]
mod tests;
