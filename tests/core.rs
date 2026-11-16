#[path = "common/mod.rs"]
pub mod common;
#[path = "common/workspace_contract_tests.rs"]
mod workspace_contract_tests;

#[path = "core/adapters.rs"]
mod adapters;
#[path = "core/build_query_roundtrip.rs"]
mod build_query_roundtrip;
#[path = "core/compact_schema.rs"]
mod compact_schema;
#[path = "core/coverage_diagnostics.rs"]
mod coverage_diagnostics;
#[path = "core/database_guard.rs"]
mod database_guard;
#[path = "core/debug_logging.rs"]
mod debug_logging;
#[path = "core/docs_check.rs"]
mod docs_check;
#[path = "core/edge_aggregation.rs"]
mod edge_aggregation;
#[path = "core/fact_persistence.rs"]
mod fact_persistence;
#[path = "core/reader_row_limits.rs"]
mod reader_row_limits;
#[cfg(unix)]
#[path = "core/release_packaging.rs"]
mod release_packaging;
#[path = "core/scanner_guard_limits.rs"]
mod scanner_guard_limits;
#[path = "core/storage_batches.rs"]
mod storage_batches;

mod incremental {
    #[path = "../incremental/directory_slice.rs"]
    mod directory_slice;
    #[path = "../incremental/doc_parity.rs"]
    mod doc_parity;
    #[path = "../incremental/module_scope.rs"]
    mod module_scope;
    #[path = "../incremental/resolution_identity.rs"]
    mod resolution_identity;
    #[path = "../incremental/scope.rs"]
    mod scope;
}
