//! Pure Rust ContextUnity ACDD core contracts, IDs, errors, path policies, configuration, and filesystem confinement.

pub mod config;
pub mod errors;
pub mod failpoint;
pub mod format;
pub mod fs;
pub mod git;
pub mod ids;
pub mod lint;
pub mod lock;
pub mod model;
pub mod path_policy;
pub mod ports;
pub mod response;
pub mod schema;
pub mod subsystems;
pub mod sys;
pub mod telemetry;

pub use fs::is_doc_path;
