//! ContextUnity ACDD AST extraction, linking, and index persistence.

pub mod ast;
pub mod commitments;
pub mod db;
pub mod docs;
pub mod interner;
pub mod languages;
pub mod linker;
pub mod models;
pub mod scanner;
pub mod semantic;
pub mod typed_facts;

pub use db::{initialize_code_map_schema, SCHEMA_DDL};
pub use interner::{LinkerInterner, PathId, SymbolId};
pub use languages::LanguageRegistry;
pub use linker::RouteIndex;
pub use models::{is_test, Coverage, Edge, Facts, FileEntry, Graph, Node, NodeSemanticMeta, Reference};
pub(crate) use models::FactsLookup;
pub use typed_facts::{NodeSemanticEntry, SemanticStore};

/// Current index semantics version combining the contract epoch and language profile digest.
pub const INDEX_SEMANTICS_VERSION: &str = concat!(
    "15:specs-contract-v1:",
    env!("ACDD_LANGUAGE_PROFILE_DIGEST")
);

/// Returns the acdd-index crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
