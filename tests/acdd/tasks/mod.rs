use super::support::*;
use contextunity_acdd::engine::milestones::recovery::db_recover;

const CANONICAL_ACDD_GITIGNORE: &str = concat!(
    "target/\n",
    "!/.acdd/\n",
    "!/.acdd/profile.yaml\n",
    "!/.acdd/frameworks/\n",
    "!/.acdd/frameworks/**\n",
    ".acdd/*.sqlite*\n",
    ".acdd/*.lock\n",
    ".acdd/*.log\n",
    ".acdd/*.jsonl\n",
    ".acdd/tasks/\n",
    ".acdd/checkpoints.json\n",
);

mod boundaries;
mod guidance;
pub(super) mod harness;
mod lifecycle;
mod mcp_fixture;
mod retirement;
mod seams;
mod workspaces;
