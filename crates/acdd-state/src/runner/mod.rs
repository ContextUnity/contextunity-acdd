//! Gate command execution, role runner spawning, and topological wave orchestration.

pub mod command;
mod process;
pub mod spawner;
pub mod waves;

#[cfg(test)]
mod tests;

pub(crate) use command::render_invocation_command;
pub use command::{execute_gate_commands, CommandExecReq};
pub use spawner::{
    spawn_role_runner_borrowed, verify_runner_submitted, RoleSpawnBorrow, RoleSpawnOutcome,
};
pub use waves::WaveExecutionReport;
