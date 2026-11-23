//! Git worktree, snapshot, scoped commit, integration, and stateless crash-recovery operations.

pub mod commit;
mod exec;
pub mod integrate;
pub mod snapshot;
pub mod worktree;

#[cfg(test)]
mod tests;
