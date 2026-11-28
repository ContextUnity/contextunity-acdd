//! Canonical SQLite DDL for the 9 pure domain tables and SQLite-private `_sqlite_meta`.

/// Current schema version seeded into `_sqlite_meta.schema_version`.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Authoritative list of the 9 pure domain tables defined in `v7` §4.1.
pub const DOMAIN_TABLES: [&str; 9] = [
    "runs",
    "gate_nodes",
    "task_dependencies",
    "subtasks",
    "gate_claims",
    "gate_traces",
    "gate_artifacts",
    "blackboard",
    "run_tombstones",
];

/// Complete version-1 DDL batch creating the 9 domain tables, all CHECK/FK/UNIQUE constraints,
/// partial unique indexes, and `_sqlite_meta`.
pub const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS _sqlite_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    write_seq BIGINT NOT NULL DEFAULT 0 CHECK (write_seq >= 0),
    last_backup_seq BIGINT NOT NULL DEFAULT 0 CHECK (last_backup_seq >= 0)
);

CREATE TABLE IF NOT EXISTS runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    run_name TEXT NOT NULL,
    collection TEXT,
    workspace TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    profile_json TEXT NOT NULL CHECK (json_valid(profile_json)),
    spec_path TEXT,
    spec_digest TEXT NOT NULL,
    is_ephemeral INTEGER NOT NULL CHECK (is_ephemeral IN (0, 1)),
    isolation TEXT NOT NULL CHECK (isolation IN ('in_place', 'worktree')),
    target_base TEXT,
    target_base_commit TEXT,
    branch TEXT,
    worktree TEXT,
    trace_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'completed', 'cancelled')),
    cancellation_reason TEXT,
    root_cycle_count INTEGER NOT NULL DEFAULT 0 CHECK (root_cycle_count >= 0),
    generation BIGINT NOT NULL DEFAULT 1 CHECK (generation >= 1),
    created_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE (workspace, run_name)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_runs_active_worktree
    ON runs(worktree) WHERE status = 'active' AND worktree IS NOT NULL;

CREATE TABLE IF NOT EXISTS gate_nodes (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    parent_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('container', 'root_gate', 'task', 'task_gate')),
    task_ref TEXT,
    gate_id TEXT NOT NULL,
    seq_order INTEGER NOT NULL CHECK (seq_order >= 0),
    status TEXT NOT NULL CHECK (status IN ('pending', 'ready', 'in_progress', 'completed', 'stale', 'escalated', 'cancelled')),
    node_version BIGINT NOT NULL DEFAULT 1 CHECK (node_version >= 1),
    isolation TEXT NOT NULL CHECK (isolation IN ('in_place', 'worktree')),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    cycle_count INTEGER NOT NULL DEFAULT 0 CHECK (cycle_count >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    claim_revision INTEGER NOT NULL DEFAULT 0 CHECK (claim_revision >= 0),
    contract_revision INTEGER NOT NULL DEFAULT 1 CHECK (contract_revision >= 1),
    worker_id TEXT,
    session_fingerprint TEXT,
    branch TEXT,
    worktree TEXT,
    baseline_commit TEXT,
    claim_tree TEXT,
    accepted_tree TEXT,
    accepted_definition_digest TEXT,
    tags_json TEXT NOT NULL DEFAULT '["default"]' CHECK (json_valid(tags_json)),
    invariants_json TEXT NOT NULL DEFAULT '{"applicable":[],"verified":[]}' CHECK (json_valid(invariants_json)),
    scope_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(scope_json)),
    extended_scope_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(extended_scope_json)),
    active_span_id TEXT,
    PRIMARY KEY (run_id, node_id),
    FOREIGN KEY (run_id, parent_id) REFERENCES gate_nodes(run_id, node_id) ON DELETE CASCADE,
    CHECK ((kind IN ('container', 'root_gate') AND task_ref IS NULL) OR (kind IN ('task', 'task_gate') AND task_ref IS NOT NULL)),
    CHECK (kind != 'task' OR gate_id = '__task__')
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_gate_nodes_task
    ON gate_nodes(run_id, task_ref) WHERE kind = 'task';
CREATE INDEX IF NOT EXISTS idx_gate_nodes_parent
    ON gate_nodes(run_id, parent_id);
CREATE INDEX IF NOT EXISTS idx_gate_nodes_status_seq
    ON gate_nodes(run_id, status, seq_order);
CREATE INDEX IF NOT EXISTS idx_gate_nodes_worktree_status
    ON gate_nodes(worktree, status) WHERE worktree IS NOT NULL;

CREATE TABLE IF NOT EXISTS task_dependencies (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    task_ref TEXT NOT NULL,
    depends_on_workspace TEXT NOT NULL,
    depends_on_run_id TEXT NOT NULL,
    depends_on_ref TEXT NOT NULL,
    satisfied INTEGER NOT NULL DEFAULT 0 CHECK (satisfied IN (0, 1)),
    CHECK (NOT (depends_on_run_id = run_id AND depends_on_ref = task_ref)),
    PRIMARY KEY (run_id, task_ref, depends_on_workspace, depends_on_run_id, depends_on_ref)
);

CREATE INDEX IF NOT EXISTS idx_task_dependencies_target
    ON task_dependencies(depends_on_workspace, depends_on_run_id, depends_on_ref);

CREATE TABLE IF NOT EXISTS subtasks (
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    subtask_ref TEXT NOT NULL,
    seq_order INTEGER NOT NULL CHECK (seq_order >= 0),
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled')),
    evidence TEXT,
    updated_at TEXT NOT NULL,
    CHECK (status NOT IN ('completed', 'cancelled') OR (evidence IS NOT NULL AND length(trim(evidence)) > 0)),
    PRIMARY KEY (run_id, node_id, subtask_ref),
    FOREIGN KEY (run_id, node_id) REFERENCES gate_nodes(run_id, node_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS gate_claims (
    claim_id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    claim_revision INTEGER NOT NULL CHECK (claim_revision >= 1),
    worker_id TEXT NOT NULL,
    session_fingerprint TEXT,
    worktree TEXT NOT NULL,
    span_id TEXT NOT NULL,
    claim_tree TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT NOT NULL CHECK (outcome IN ('active', 'passed', 'rejected', 'superseded', 'reopened', 'cancelled')),
    CHECK ((outcome = 'active' AND ended_at IS NULL) OR (outcome != 'active' AND ended_at IS NOT NULL)),
    UNIQUE (run_id, node_id, claim_revision),
    FOREIGN KEY (run_id, node_id) REFERENCES gate_nodes(run_id, node_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_gate_claims_one_active
    ON gate_claims(run_id, node_id) WHERE outcome = 'active';

CREATE TABLE IF NOT EXISTS gate_traces (
    trace_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    claim_revision INTEGER NOT NULL CHECK (claim_revision >= 1),
    contract_revision INTEGER NOT NULL CHECK (contract_revision >= 1),
    execution_digest TEXT NOT NULL,
    idempotency_key TEXT,
    command_id TEXT,
    command_line TEXT,
    action_id TEXT,
    test_target TEXT,
    status TEXT NOT NULL CHECK (status IN ('running', 'passed', 'expected_failure', 'failed', 'timed_out', 'compile_error', 'check_failed', 'scope_violation', 'aborted')),
    exit_code INTEGER,
    tests_passed INTEGER NOT NULL DEFAULT 0 CHECK (tests_passed >= 0),
    tests_failed INTEGER NOT NULL DEFAULT 0 CHECK (tests_failed >= 0),
    tests_ignored INTEGER NOT NULL DEFAULT 0 CHECK (tests_ignored >= 0),
    passed_tests_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(passed_tests_json)),
    failed_tests_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(failed_tests_json)),
    check_results_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(check_results_json)),
    candidate_tree_before TEXT NOT NULL,
    candidate_tree_after TEXT,
    log_relpath TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    CHECK ((status = 'running' AND finished_at IS NULL AND candidate_tree_after IS NULL) OR (status != 'running' AND finished_at IS NOT NULL)),
    FOREIGN KEY (run_id, node_id) REFERENCES gate_nodes(run_id, node_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_gate_traces_one_running
    ON gate_traces(run_id, node_id, claim_revision) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS gate_artifacts (
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    claim_revision INTEGER NOT NULL CHECK (claim_revision >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('proof', 'findings', 'receipt', 'accepted_snapshot')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    snapshot_ref TEXT,
    snapshot_commit TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (run_id, node_id, claim_revision, kind),
    FOREIGN KEY (run_id, node_id) REFERENCES gate_nodes(run_id, node_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS blackboard (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    node_id TEXT,
    recipient_node_id TEXT,
    subtask_ref TEXT,
    gate TEXT,
    author TEXT NOT NULL,
    topic TEXT NOT NULL CHECK (topic IN ('draft', 'notes', 'findings', 'blockers', 'decisions', 'deferred')),
    status TEXT NOT NULL CHECK (status IN ('open', 'acknowledged', 'resolved', 'carried_over')),
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_blackboard_run_topic_id
    ON blackboard(run_id, topic, id);
CREATE INDEX IF NOT EXISTS idx_blackboard_run_recipient_status_id
    ON blackboard(run_id, recipient_node_id, status, id);

CREATE TABLE IF NOT EXISTS run_tombstones (
    run_id TEXT PRIMARY KEY NOT NULL,
    run_name TEXT NOT NULL,
    collection TEXT,
    workspace TEXT NOT NULL,
    spec_id TEXT,
    title TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    is_ephemeral INTEGER NOT NULL CHECK (is_ephemeral IN (0, 1)),
    generation BIGINT NOT NULL CHECK (generation >= 1),
    terminal_status TEXT NOT NULL CHECK (terminal_status IN ('completed', 'cleared')),
    target_base TEXT,
    target_base_commit TEXT,
    archive_path TEXT,
    closed_commit TEXT,
    cleanup_pending INTEGER NOT NULL DEFAULT 1 CHECK (cleanup_pending IN (0, 1)),
    cleanup_manifest_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(cleanup_manifest_json)),
    summary_json TEXT NOT NULL CHECK (json_valid(summary_json)),
    metrics_json TEXT NOT NULL CHECK (json_valid(metrics_json)),
    closed_at TEXT NOT NULL
);
"#;
