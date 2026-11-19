//! Crate-level integration test suites for `acdd-state` (`test-support` enabled).

mod state_integration {
    pub mod common;
    pub mod git_ops;
    pub mod machine;
    pub mod roadmap_query_waves;
    pub mod runner;
    pub mod usecases_e2e;
    pub mod usecases_queries_lifecycle;
}
