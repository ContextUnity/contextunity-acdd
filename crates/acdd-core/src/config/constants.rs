//! Shared adapter configuration constants.

/// Canonical workspace adapter configuration filename.
pub const ACDD_CONFIG_FILE: &str = "acdd.yaml";
/// Canonical workspace state/metadata directory relative to the workspace root.
pub const ACDD_DIR: &str = ".acdd";
/// Canonical directory holding ACDD-managed Git worktrees relative to the primary root.
pub const WORKTREES_DIR: &str = ".worktrees";
/// Default state database path relative to primary worktree root.
pub const DEFAULT_STATE_DB: &str = ".acdd/state.sqlite";
/// Default code-map database path relative to workspace root.
pub const DEFAULT_CODE_DB: &str = ".acdd/code-map.sqlite";
/// Canonical Git ref prefix for ACDD workspace snapshots.
pub const SNAPSHOT_REF_PREFIX: &str = "refs/acdd/snapshots/";
/// Canonical Git commit trailer key recording ACDD claim attribution.
pub const CLAIM_TRAILER_KEY: &str = "Acdd-Claim";

/// Default maximum number of files admitted for indexing.
pub const DEFAULT_MAX_FILES: usize = 100_000;
/// Default maximum size in bytes for a single indexable source file (5 MiB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Default maximum total size in bytes for all indexable files (500 MiB).
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// Alias for maximum allowed file bytes.
pub const MAX_FILE_BYTES: u64 = DEFAULT_MAX_FILE_BYTES;
/// Authoritative maximum size in bytes for metadata and notes.
pub const MAX_METADATA_BYTES: usize = 2048;
/// Default maximum size in bytes for metadata and notes.
pub const DEFAULT_MAX_METADATA_BYTES: usize = MAX_METADATA_BYTES;
/// Default number of retained run trace logs.
pub const DEFAULT_RETAIN_TRACES: usize = 20;
/// Default specification ID numeric width.
pub const DEFAULT_SPEC_DIGITS: usize = 3;
/// Default directory searched for external spec definitions (`specs_dirs`).
pub const DEFAULT_SPECS_DIR: &str = ".acdd/specs";
/// Default directory searched for custom workflow definitions (`workflows_dirs`).
pub const DEFAULT_WORKFLOWS_DIR: &str = ".acdd/workflows";
/// Default directory searched for externalized lint/architecture rules (`lint_dirs`).
pub const DEFAULT_LINT_DIR: &str = ".acdd/lint";

/// Current engine schema version stored in database metadata.
pub const ENGINE_SCHEMA_VERSION: &str = "15";

/// Standard directory and file names ignored during scanning.
pub const DEFAULT_IGNORED_NAMES: &[&str] = &[
    ".git",
    ".acdd",
    ".venv",
    "__pycache__",
    "target",
    "node_modules",
    "build",
    "dist",
    "coverage",
    ".cache",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    "vendor",
    ".cargo",
    ".idea",
    ".vscode",
    ".tox",
    ".turbo",
    "out",
    ".output",
];
