//! Domain-agnostic workflow, specification, pure state machine, and SQLite backend engine for ACDD.

#![warn(missing_docs)]

/// Claim context assembly with 6-tier budget reservation and prompt rendering.
pub mod context;
/// Git worktree, snapshot, scoped commit, integration, and crash-recovery operations.
pub mod git_ops;
/// In-memory and disk test harness for state transitions and storage invariants.
#[cfg(any(test, feature = "test-support"))]
pub mod harness;
/// 100% pure functional state-machine transitions and DAG scheduling core.
pub mod machine;
/// Pure run closure digests, metrics rollups, and Prometheus 0.0.4 exposition formatting.
pub mod observability;
/// Gate command execution, role runner spawning, and topological wave orchestration.
pub mod runner;
/// Portable storage backend traits, SQLite persistence, and in-memory test backend.
pub mod store;
/// Canonical domain enums, storage records, query pagination DTOs, and transition contracts.
pub mod types;
/// Application use cases (`StateEngine`, `StateQueryEngine`, `StateCommand`, `StateQuery`, DTOs).
pub mod usecases;
/// Single canonical workspace, specification, workflow, and dependency validator.
pub mod validate;
/// Version-2 `WorkflowDef` parser, inheritance/patch resolver, and fail-closed validator.
pub mod workflow;
