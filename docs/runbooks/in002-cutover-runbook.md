# IN-002 Offline Self-Hosting Cutover Runbook

Operational procedure governing `IN-002-contextunity-acdd-one-pass-rename` across Waves 0-3 and the post-delivery switch to `acdd` / `.acdd/`.

## 1. Pinned Controller, Checksum, & Worktree DB Ownership (Waves 0-3)

- **Pinned Pre-Rename Binary**: Capture the pre-rename `contextunity-forge-mcp` executable outside `target/`, `.forge/`, and `.acdd/` (installed `~/.local/bin/contextunity-forge-mcp` and/or pinned copy `/tmp/in002-controller/contextunity-forge-mcp`), and verify its SHA-256 checksum and version before Wave 1:
  ```bash
  sha256sum /tmp/in002-controller/contextunity-forge-mcp ~/.local/bin/contextunity-forge-mcp
  /tmp/in002-controller/contextunity-forge-mcp --version
  ```
- **Authoritative Primary-Worktree DB Ownership**: Linked task worktrees resolve the shared `tasks.sqlite` under `primary_worktree_root` via `TaskSettings::path` (`crates/acdd-tasks/src/engine/mod.rs`). Never relocate, merge, or hand-edit `.forge/tasks.sqlite` while Waves 0-3 are active.
- **Explicit Operational Config & Profile Overrides (Untracked, Never Shipped)**:
  - Normal lifecycle coordination via the reloaded `contextunity-forge` MCP server (`task_claim`, `task_manage`, `task_submit`, `task_blackboard`) operates on `.forge/tasks.sqlite` directly while `.forge/acdd/profile.yaml` remains tracked.
  - Once Wave 1 renames `crates/forge-*` -> `crates/acdd-*` and `[[bin]] name = "acdd"` (before Wave 3 switches tracked config/profile), invoke the pinned controller with explicit gitignored operational files under `.forge-mcp/`:
    ```bash
    /tmp/in002-controller/contextunity-forge-mcp task <subcommand> \
      --root . \
      --config .forge-mcp/in002-config.yaml \
      --profile .forge-mcp/in002-profile.yaml
    ```
  - `.forge-mcp/in002-config.yaml` binds `acdd_profile: .forge-mcp/in002-profile.yaml` and preserves primary-worktree `.forge/tasks.sqlite` (`repository: forge-mcp`).
  - `.forge-mcp/in002-profile.yaml` sets `version: 2`, `inherit: default`, retains command IDs (`rust-check-and-test`, `check_full_repo`), and targets renamed crates/bin in `scope_args_by_pattern`:
    - `crates/acdd-core/**/*.rs`: `--package acdd-core --lib`
    - `crates/acdd-index/**/*.rs`: `--package acdd-index --lib`
    - `crates/acdd-tasks/**/*.rs`: `--package acdd-tasks --lib`
    - `src/main.rs`: `--bin acdd`
    - `tests/acdd/**/*.rs`: `--test acdd`
  - Delete `.forge-mcp/` after cutover finishes; never commit or ship it as a fallback.

## 2. Offline Switch Procedure (After Wave 3 `deliver` Only)

1. Confirm Wave 3 `activate-config-and-profile` has accepted `deliver` and its isolated continuity rehearsal (`cargo test --test acdd milestones::handoff`) has passed without creating `.forge/` or a second task store.
2. Freeze new claims and quiesce all open writers (`contextunity-forge-mcp` MCP daemons, CLI sessions, SQLite handles) across primary and linked worktrees.
3. Resolve any open lifecycle checkpoints using supported recovery commands before moving state files.
4. Inventory counts/identities (tasks, receipts, blackboard messages, root cursor, checkpoint ownership, snapshot refs), checkpoint and close SQLite via supported APIs, and save a read-only offline backup outside `.forge/` and `.acdd/`.
5. Fail closed if `.acdd/tasks.sqlite` or unrelated destination operational state is already occupied, or if an unresolved checkpoint remains open.
6. Execute ownership-aware operational entry transfer from `.forge/` into `.acdd/` at the primary owning root (plus required worktree-local runtime dirs) **preserving** already-tracked `.acdd/profile.yaml` and `.acdd/frameworks/**`. Never overwrite tracked destination config/framework files, never merge two SQLite databases, and never unconditionally rewrite `execution_traces.log_path` SQL rows. If an indispensable old locator prevents consumption by `acdd`, stop and return the authority gap to the architect without adding compatibility translators.
7. Rebuild disposable code indexes/caches with `acdd`, verify `acdd milestone list`, `acdd task list`, and `acdd task blackboard` consume unchanged accepted evidence without recreating `.forge/`, complete final `milestone verify` and `handoff` (`pub fn handoff(root: &Path, selector: &str, commit: Option<&str>, evidence: &IntegrationEvidence)`), and archive the milestone.

`PRIMARY_ROOT` is the main worktree and owns the task database. It retains its
tracked `forge-mcp.yaml` and `.forge/acdd/profile.yaml` until final landing.
`MILESTONE_WT` contains the delivered `acdd.yaml`, `.acdd/profile.yaml`, and
milestone manifest. The new controller reads those files from `MILESTONE_WT`
while `TaskSettings::path` resolves the database in `PRIMARY_ROOT`.

Stop every controller, CLI and SQLite writer in all worktrees. Freeze new claims throughout the procedure. Inspect this process list
and stop if any writer remains; it cannot prevent another writer from starting:

```bash
set -e
PRIMARY_ROOT="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
MILESTONE_WT="$PRIMARY_ROOT/.worktrees/IN-002-contextunity-acdd-one-pass-rename"
export PRIMARY_ROOT MILESTONE_WT
MID=IN-002-contextunity-acdd-one-pass-rename
BACKUP=/tmp/in002-offline-backup
export BACKUP
pgrep -af 'contextunity-forge-mcp|(^|/)acdd( |$)' || true
/tmp/in002-controller/contextunity-forge-mcp --root "$MILESTONE_WT" \
  --config .forge-mcp/in002-config.yaml --profile .forge-mcp/in002-profile.yaml \
  milestone db-recover "$MID"
```

Stop if recovery fails. The following holds the receipt fence, checks the
database, takes an offline backup, and moves only nonconflicting entries on the
same filesystem. It refuses historical `.forge/` trace locators: return those
to the architect instead of rewriting accepted evidence. Reconcile a
destination collision before retrying. Run only after all
writers are stopped and the pre-transfer inventory is recorded.

```bash
set -e
cd "$PRIMARY_ROOT"
flock -n .forge/tasks.receipt.lock python3 - <<'PY'
import os, pathlib, shutil, sqlite3, sys
src, dst = pathlib.Path('.forge'), pathlib.Path('.acdd')
backup = pathlib.Path(os.environ['BACKUP'])
def require(condition, message):
    if not condition: sys.exit(message)
require(src.is_dir() and (src / 'tasks.sqlite').is_file(), 'missing source store')
require(not backup.exists(), 'backup already exists')
require(not (dst / 'tasks.sqlite').exists(), 'destination store occupied')
names = ['tasks.sqlite', 'tasks.sqlite-wal', 'tasks.sqlite-shm', 'tasks.sqlite.lock', 'checkpoints.json', 'tasks', 'task-traces']
require(not any(p.is_symlink() for n in names for p in ([src / n] + list((src / n).rglob('*')) if (src / n).is_dir() else [src / n])), 'operational state contains symlinks')
dst.mkdir(exist_ok=True)
def entries(): return [src / name for name in names if (src / name).exists()]
for p in entries(): require(not (dst / p.name).exists(), f'destination occupied: {p.name}')
con = sqlite3.connect(f'file:{src / "tasks.sqlite"}?mode=rw', uri=True, timeout=0)
try:
    require(con.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', 'invalid store')
    require(con.execute("SELECT value FROM task_store_metadata WHERE key='schema_version'").fetchone(), 'missing schema')
    require(not con.execute("SELECT EXISTS(SELECT 1 FROM task_store_metadata WHERE key='active_checkpoint_pending')").fetchone()[0], 'checkpoint pending')
    require(not con.execute("SELECT EXISTS(SELECT 1 FROM gate_execution_traces WHERE log_path LIKE '.forge/%' OR log_path LIKE '%/.forge/%')").fetchone()[0], 'old trace locator requires architect resolution')
    require(con.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0, 'checkpoint busy')
finally:
    con.close()
items = entries()
for p in items: require(not (dst / p.name).exists(), f'destination occupied: {p.name}')
require(os.stat(src).st_dev == os.stat(dst).st_dev, 'cross-filesystem transfer refused')
shutil.copytree(src, backup)
(backup / 'transfer-entries.txt').write_text(''.join(p.name + '\n' for p in items))
for p in items: os.rename(p, dst / p.name)
PY
```

If any rename fails, do not start `acdd`; use Section 3. Preserve the backup
until handoff is verified. Handle inventoried worktree-local state separately
under the same collision and ownership rules.


Run the following from the milestone worktree after transfer, using the
built new controller and a reviewed integration evidence file. Capture and
compare the outputs with the pre-transfer inventory before handoff. The
handoff-context command may absorb milestone adjustments, so run it only after
all task candidates have been delivered and reviewed.

```bash
set -e
cd "$MILESTONE_WT"
cargo build --release --bin acdd
NEW_ACDD="$MILESTONE_WT/target/release/acdd"
test -x "$NEW_ACDD"
"$NEW_ACDD" --root "$MILESTONE_WT" build .
"$NEW_ACDD" --root "$MILESTONE_WT" milestone list
"$NEW_ACDD" --root "$MILESTONE_WT" task list --milestone IN-002-contextunity-acdd-one-pass-rename --status all
"$NEW_ACDD" --root "$MILESTONE_WT" task blackboard read --scope milestone --milestone-ref IN-002-contextunity-acdd-one-pass-rename
"$NEW_ACDD" --root "$MILESTONE_WT" --format json milestone handoff-context --target-base main "$MID" > /tmp/in002-binding.json
"$NEW_ACDD" --root "$MILESTONE_WT" --format json milestone verify "$MID" --target-base main > /tmp/in002-verification.json
cargo test -p acdd-tasks --lib
```

After independent review, set `REVIEWER` to the reviewer's identity and
`SUMMARY` to the passing review summary; confirm that the reviewer is distinct
from all builders and that there are no findings. Construct and inspect the
evidence before handoff:

```bash
: "${REVIEWER:?Set REVIEWER to the independent reviewer identity}"
: "${SUMMARY:?Set SUMMARY to the passing review summary}"
export REVIEWER SUMMARY
python3 - <<'PY'
import json, os
from pathlib import Path
context = json.loads(Path('/tmp/in002-binding.json').read_text())
check = json.loads(Path('/tmp/in002-verification.json').read_text())['verification']
evidence = {'binding': context['binding'], 'target_base': context['target_base'],
    'review': {'worker_id': os.environ['REVIEWER'], 'decision': 'pass',
        'summary': os.environ['SUMMARY'], 'scope_findings': [], 'contract_findings': []},
    'verification': {key: check[key] for key in ('command', 'exit_code', 'tests_passed', 'tests_failed')}}
evidence['verification']['project_command'] = check['command_id']
Path(os.environ['MILESTONE_WT'], '.acdd/integration-evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
PY
cat "$MILESTONE_WT/.acdd/integration-evidence.json"
"$NEW_ACDD" --root "$MILESTONE_WT" milestone handoff IN-002-contextunity-acdd-one-pass-rename --evidence-file "$MILESTONE_WT/.acdd/integration-evidence.json"
```

Construct the evidence file from the returned binding and verification results
and an independent review, as specified in the milestone handoff procedure.

## 3. Interruption Recovery

Before any new-controller write, stop all writers and restore the offline backup
using the transfer inventory. If `acdd` has already written, do not restore
stale bytes; use supported recovery instead. Run from the primary worktree:

```bash
export BACKUP=/tmp/in002-offline-backup
cd "$PRIMARY_ROOT"
python3 - <<'PY'
import os, pathlib, shutil, sys
src, dst = pathlib.Path('.forge'), pathlib.Path('.acdd')
backup = pathlib.Path(os.environ['BACKUP'])
manifest = backup / 'transfer-entries.txt'
if not manifest.is_file(): sys.exit('missing transfer inventory')
names = manifest.read_text().splitlines()
if any(not n or pathlib.Path(n).name != n for n in names): sys.exit('invalid inventory')
src.mkdir(exist_ok=True)
for name in names:
    if (src / name).exists() and (dst / name).exists(): sys.exit(f'duplicate: {name}')
    if not (backup / name).exists(): sys.exit(f'missing backup: {name}')
for name in names:
    for path in (src / name, dst / name):
        if path.is_dir(): shutil.rmtree(path)
        elif path.exists(): path.unlink()
    saved, restored = backup / name, src / name
    if saved.is_dir(): shutil.copytree(saved, restored)
    else: shutil.copy2(saved, restored)
PY
```

Verify the restored inventory, recover pending checkpoints with the pinned
controller if necessary, and move the backup aside before retrying Section 2.

After successful handoff, remove the temporary `.forge-mcp/` overrides when no
longer needed. Final `git merge --ff-only` in `PRIMARY_ROOT` lands the milestone
and removes the old tracked config and profile from main through Git; neither
the transfer nor rollback touches those tracked files or the receipt lock.
