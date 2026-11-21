//! Portable storage backend traits, SQLite persistence, and in-memory test backend.

/// In-memory SQLite-backed backend for fast unit and state-machine test matrices.
#[cfg(any(test, feature = "test-support"))]
pub mod memory;
/// Disk-backed SQLite state engine (`rusqlite` isolated strictly inside this module).
pub mod sqlite;
/// Backend-agnostic `StateBackend`, `StateReader`, and `StateTx` traits.
pub mod traits;

#[cfg(any(test, feature = "test-support"))]
pub use memory::MemoryStateBackend;
pub use sqlite::{SqliteBackendOptions, SqliteStateBackend, SqliteStateReader};
pub use traits::{exec_tx_with, StateBackend, StateReader, StateTx};

/// Delegate all [`StateReader`] methods on `$ty` to an inner reader or `with_reader` store.
macro_rules! impl_state_reader_delegate {
    (@call $self_val:expr, $self_ident:ident, direct, $target:expr, $method:ident($($arg:expr),*)) => {{
        let $self_ident = $self_val;
        $target.$method($($arg),*)
    }};
    (@call $self_val:expr, $self_ident:ident, with_reader, $target:expr, $method:ident($($arg:expr),*)) => {{
        let $self_ident = $self_val;
        $target.with_reader(|conn| $crate::store::sqlite::reader::$method(conn $(, $arg)*))
    }};
    ($ty:ty, |$self_ident:ident| $reader_expr:expr) => {
        $crate::store::impl_state_reader_delegate!(@impl $ty, direct, |$self_ident| $reader_expr);
    };
    ($ty:ty, with_reader |$self_ident:ident| $store_expr:expr) => {
        $crate::store::impl_state_reader_delegate!(@impl $ty, with_reader, |$self_ident| $store_expr);
    };
    (@impl $ty:ty, $mode:ident, |$self_ident:ident| $target:expr) => {
        impl $crate::store::traits::StateReader for $ty {
            fn load_task_statuses(
                &self,
            ) -> Result<Vec<(acdd_core::ids::RunId, String, $crate::types::NodeStatus)>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, load_task_statuses())
            }

            fn load_run_graph(
                &self,
                run_id: &acdd_core::ids::RunId,
            ) -> Result<Option<$crate::types::RunGraphSnapshot>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, load_run_graph(run_id))
            }

            fn list_runs(
                &self,
                req: &$crate::types::ListRunsReq,
            ) -> Result<$crate::types::ListRunsPage, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, list_runs(req))
            }

            fn resolve_active_run_by_worktree(
                &self,
                worktree: &str,
            ) -> Result<Option<$crate::types::RunRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, resolve_active_run_by_worktree(worktree))
            }

            fn load_node(
                &self,
                run_id: &acdd_core::ids::RunId,
                node_id: &str,
            ) -> Result<Option<$crate::types::GateNodeRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, load_node(run_id, node_id))
            }

            fn latest_claim(
                &self,
                run_id: &acdd_core::ids::RunId,
                node_id: &str,
            ) -> Result<Option<$crate::types::GateClaimRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, latest_claim(run_id, node_id))
            }

            fn latest_trace(
                &self,
                run_id: &acdd_core::ids::RunId,
                node_id: &str,
                claim_revision: u32,
            ) -> Result<Option<$crate::types::GateTraceRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, latest_trace(run_id, node_id, claim_revision))
            }

            fn load_artifact(
                &self,
                run_id: &acdd_core::ids::RunId,
                node_id: &str,
                claim_revision: u32,
                kind: $crate::types::ArtifactKind,
            ) -> Result<Option<$crate::types::GateArtifactRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, load_artifact(run_id, node_id, claim_revision, kind))
            }

            fn list_blackboard(
                &self,
                run_id: &acdd_core::ids::RunId,
                req: &$crate::types::BlackboardListReq,
            ) -> Result<$crate::types::BlackboardPage, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, list_blackboard(run_id, req))
            }

            fn load_tombstone(
                &self,
                run_id: &acdd_core::ids::RunId,
            ) -> Result<Option<$crate::types::RunTombstoneRecord>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, load_tombstone(run_id))
            }

            fn max_spec_number(
                &self,
                collection: &str,
                prefix: &str,
            ) -> Result<Option<u32>, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, max_spec_number(collection, prefix))
            }

            fn observability_snapshot(
                &self,
            ) -> Result<$crate::types::ObservabilitySnapshot, acdd_core::errors::StateError> {
                $crate::store::impl_state_reader_delegate!(@call self, $self_ident, $mode, $target, observability_snapshot())
            }
        }
    };
}

pub(crate) use impl_state_reader_delegate;
