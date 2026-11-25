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

pub(crate) use models::FactsLookup;
pub use models::INDEX_SEMANTICS_VERSION;
