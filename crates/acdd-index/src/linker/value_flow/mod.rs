//! Value-flow evaluation and type target resolution.

mod evaluate;
mod initialize;
mod lookup;
pub mod types;

pub(in crate::linker) use types::{node_position, resolve_type};
pub(crate) use types::{valid_scope_facts, MAX_CANDIDATE_BRANCH_JOINS};
pub use types::{
    ConfiguredCallResult, ConfiguredNominalReceiver, ConfiguredReceiverAuthority, RustWrapperKind,
    SemanticLimits, SemanticResolver, Symbol, SymbolRole, TypeTarget, ValueFlowIndex,
};

#[cfg(test)]
mod tests;
