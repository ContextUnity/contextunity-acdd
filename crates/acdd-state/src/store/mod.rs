//! Portable storage backend traits, SQLite persistence, and in-memory test backend.

/// In-memory SQLite-backed backend for fast unit and state-machine test matrices.
pub mod memory;
/// Disk-backed SQLite state engine (`rusqlite` isolated strictly inside this module).
pub mod sqlite;
/// Backend-agnostic `StateBackend`, `StateReader`, and `StateTx` traits.
pub mod traits;

pub use memory::MemoryStateBackend;
pub use sqlite::{SqliteBackendOptions, SqliteStateBackend, SqliteStateReader};
pub use traits::{exec_tx_with, StateBackend, StateReader, StateTx};
