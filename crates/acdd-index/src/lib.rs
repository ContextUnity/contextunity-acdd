//! ContextUnity ACDD AST extraction, linking, and index persistence.

#![allow(dead_code, unused_imports)]

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

pub use ast::AstToken;
pub use commitments::{DomainCommitment, MerkleCommitment};
pub use db::{initialize_code_map_schema, CODE_MAP_SCHEMA_VERSION, SCHEMA_DDL};
pub use interner::{LinkerInterner, PathId, SymbolId};
pub use languages::LanguageRegistry;
pub use linker::RouteIndex;
pub use models::{is_test, Coverage, Edge, Facts, FileEntry, Graph, Node, NodeSemanticMeta, Reference};
pub(crate) use models::FactsLookup;
pub use typed_facts::{NodeSemanticEntry, SemanticStore};

// Compatibility module aliases for internal references across extracted modules
pub(crate) mod core {
    pub use crate::{interner, models, semantic, typed_facts};
    pub use acdd_core::*;
    pub mod commitments {
        pub use crate::commitments::*;
        pub use crate::db::commitments::*;
    }
    pub mod schema {
        pub use crate::db::schema::*;
    }
}

pub(crate) mod engine {
    pub use crate::ast;
    pub use crate::docs;
    pub use crate::languages;
    pub use crate::linker;
    pub use crate::scanner;
}

/// Current index semantics version combining the contract epoch and language profile digest.
pub const INDEX_SEMANTICS_VERSION: &str = concat!(
    "14:specs-contract-v1:",
    env!("ACDD_LANGUAGE_PROFILE_DIGEST")
);

/// Returns the acdd-index crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
