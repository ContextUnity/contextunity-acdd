#!/usr/bin/env bash
set -euo pipefail

# Synchronize the current workspace (committed HEAD + uncommitted edits) to an isolated
# directory on the macOS SSH host and run Rust Clippy + test suites without touching
# any existing manual checkout (`~/contextunity-acdd`) or relying on remote `git push`.
#
# Usage:
#   ./scripts/test-macos-ssh.sh
#   ./scripts/test-macos-ssh.sh --scope crates/acdd-state/src/profile/mod.rs [--filter <pat>]
#   ./scripts/test-macos-ssh.sh --target "--package acdd-state --test state_integration" --filter <pat> [--exact] [--no-clippy] [-- --nocapture]

usage() {
    cat <<'USAGE'
Usage: test-macos-ssh.sh [OPTIONS] [-- EXTRA_TEST_ARGS...]

Run Clippy and Rust tests on the remote macOS SSH host against the current workspace.
With no target/scope options, runs full workspace verification:
  cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-targets

Options:
  --scope <path>      Resolve a workspace file/dir path to its Cargo target args (repeatable),
                      mirroring `.acdd/profile.yaml`'s `scope_args_by_pattern` + crate tests.
  --target <args>     Explicit Cargo target args, e.g. "--package acdd-state --lib" (repeatable).
  --filter <pattern>  Cargo test name/module filter passed to `cargo test`.
  --exact             Require exact test name match (passes `--exact` to the test binary).
  --no-clippy         Skip `cargo clippy` for fast targeted test iteration.
  -h, --help          Show this help message and exit.
USAGE
}

resolve_scope_to_target() {
    local raw="$1"
    local p="${raw#./}"
    p="${p//\\//}"
    p="${p%/}"

    case "$p" in
        src/main.rs)
            printf '%s\n' "--bin acdd"
            ;;
        src/*.rs|src/*/*.rs|src/*/*/*.rs|src/*/*/*/*.rs|src)
            printf '%s\n' "--lib"
            ;;
        crates/*/src|crates/*/src/*.rs|crates/*/src/*/*.rs|crates/*/src/*/*/*.rs)
            local crate="${p#crates/}"
            crate="${crate%%/*}"
            printf '%s\n' "--package ${crate} --lib"
            ;;
        crates/acdd-state/tests/state_integration|crates/acdd-state/tests/state_integration.rs|crates/acdd-state/tests/state_integration/*.rs|crates/acdd-state/tests/state_integration/*/*.rs)
            printf '%s\n' "--package acdd-state --test state_integration"
            ;;
        crates/*/tests/*.rs)
            local crate="${p#crates/}"
            crate="${crate%%/*}"
            local stem="${p##*/}"
            stem="${stem%.rs}"
            printf '%s\n' "--package ${crate} --test ${stem}"
            ;;
        tests/acdd|tests/acdd/*.rs|tests/acdd/*/*.rs)
            printf '%s\n' "--test acdd"
            ;;
        tests/common|tests/common/*.rs|tests/common/*/*.rs)
            printf '%s\n' "--all-targets"
            ;;
        tests/commitment_integrity|tests/commitment_integrity/*.rs|tests/commitment_integrity/*/*.rs)
            printf '%s\n' "--test commitment_integrity"
            ;;
        tests/core|tests/core/*.rs|tests/core/*/*.rs|tests/incremental|tests/incremental/*.rs|tests/incremental/*/*.rs)
            printf '%s\n' "--test core"
            ;;
        tests/languages|tests/languages/*.rs|tests/languages/*/*.rs|tests/framework_manifests|tests/framework_manifests/*.rs|tests/framework_manifests/*/*.rs)
            printf '%s\n' "--test languages"
            ;;
        tests/linker|tests/linker/*.rs|tests/linker/*/*.rs|tests/receivers|tests/receivers/*.rs|tests/receivers/*/*.rs)
            printf '%s\n' "--test linker"
            ;;
        tests/mcp|tests/mcp/*.rs|tests/mcp/*/*.rs|tests/mcp_context|tests/mcp_context/*.rs|tests/mcp_context/*/*.rs)
            printf '%s\n' "--test mcp"
            ;;
        tests/query_context|tests/query_context/*.rs|tests/query_context/*/*.rs)
            printf '%s\n' "--test query_context"
            ;;
        tests/*.rs)
            local stem="${p#tests/}"
            stem="${stem%.rs}"
            printf '%s\n' "--test ${stem}"
            ;;
        *)
            echo "error: scope path '$raw' does not match a known Rust test target (pass --target \"...\")" >&2
            return 1
            ;;
    esac
}

TARGETS=()
FILTER=""
EXACT=0
NO_CLIPPY=0
EXTRA_TEST_ARGS=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --scope)
            if [ "$#" -lt 2 ]; then
                echo "error: --scope requires a path argument" >&2
                exit 1
            fi
            resolved="$(resolve_scope_to_target "$2")"
            seen=0
            for existing in "${TARGETS[@]+"${TARGETS[@]}"}"; do
                if [ "$existing" = "$resolved" ]; then
                    seen=1
                    break
                fi
            done
            if [ "$seen" -eq 0 ]; then
                TARGETS+=("$resolved")
            fi
            shift 2
            ;;
        --target)
            if [ "$#" -lt 2 ]; then
                echo "error: --target requires Cargo target arguments" >&2
                exit 1
            fi
            TARGETS+=("$2")
            shift 2
            ;;
        --filter)
            if [ "$#" -lt 2 ]; then
                echo "error: --filter requires a pattern argument" >&2
                exit 1
            fi
            FILTER="$2"
            shift 2
            ;;
        --exact)
            EXACT=1
            shift
            ;;
        --no-clippy)
            NO_CLIPPY=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            EXTRA_TEST_ARGS+=("$@")
            break
            ;;
        *)
            echo "error: unknown argument '$1' (run with --help)" >&2
            exit 1
            ;;
    esac
done

if [ "$EXACT" -eq 1 ] && [ -z "$FILTER" ]; then
    echo "error: --exact requires --filter <pattern>" >&2
    exit 1
fi

if [ "${#TARGETS[@]}" -eq 0 ]; then
    TARGETS=("--workspace --all-targets")
fi

SSH_HOST="${ACDD_MACOS_SSH_HOST:-maxos}"
REMOTE_DIR="${ACDD_MACOS_REMOTE_DIR:-contextunity-acdd-ci}"

if ! command -v ssh >/dev/null 2>&1 || ! command -v rsync >/dev/null 2>&1; then
    echo "error: required tools 'ssh' and 'rsync' must be installed locally" >&2
    exit 1
fi

if ! ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$SSH_HOST" true; then
    echo "error: non-interactive SSH connection to '$SSH_HOST' failed (check SSH config/agent/host)" >&2
    exit 1
fi

REMOTE_HOME="$(ssh -n -o BatchMode=yes "$SSH_HOST" 'printf %s "$HOME"')"
if [ -z "$REMOTE_HOME" ]; then
    echo "error: unable to resolve \$HOME on '$SSH_HOST'" >&2
    exit 1
fi

case "$REMOTE_DIR" in
    /*) REMOTE_ABS="$REMOTE_DIR" ;;
    ~/*) REMOTE_ABS="$REMOTE_HOME/${REMOTE_DIR#~/}" ;;
    *) REMOTE_ABS="$REMOTE_HOME/$REMOTE_DIR" ;;
esac
LOCK_DIR="${REMOTE_ABS}.acdd-lock"
STALE_SECS="${ACDD_MACOS_LOCK_STALE_SECS:-90}"
case "$STALE_SECS" in
    ''|*[!0-9]*|0)
        echo "error: ACDD_MACOS_LOCK_STALE_SECS must be a positive integer (got '$STALE_SECS')" >&2
        exit 1
        ;;
esac

LOCAL_HOST="$(hostname -s 2>/dev/null || hostname 2>/dev/null || printf 'unknown')"
LOCK_OWNER="${LOCAL_HOST}-$$-$(date +%s)"
LOCK_ACQUIRED=0
HEARTBEAT_PID=""
PARENT_PID="$$"

cleanup_remote_lock() {
    local exit_code=$?
    trap - EXIT INT TERM
    if [ -n "$HEARTBEAT_PID" ] && kill -0 "$HEARTBEAT_PID" 2>/dev/null; then
        kill "$HEARTBEAT_PID" 2>/dev/null || true
        wait "$HEARTBEAT_PID" 2>/dev/null || true
    fi
    if [ "$LOCK_ACQUIRED" -eq 1 ]; then
        local remote_unlock_cmd=""
        printf -v remote_unlock_cmd 'if [ -d %q ] && [ "$(cat %q 2>/dev/null || true)" = %q ]; then rm -rf %q; fi' \
            "$LOCK_DIR" "$LOCK_DIR/owner" "$LOCK_OWNER" "$LOCK_DIR"
        ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$SSH_HOST" "$remote_unlock_cmd" >/dev/null 2>&1 || true
    fi
    return "$exit_code"
}
trap cleanup_remote_lock EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Serialize concurrent local runs sharing the same warm remote `target/` cache,
# storing owner/heartbeat inside `$LOCK_DIR` and reclaiming stale locks when idle.
ssh -T -o BatchMode=yes "$SSH_HOST" bash -s -- "$LOCK_DIR" "$REMOTE_ABS" "$LOCK_OWNER" "$STALE_SECS" <<'EOF'
set -euo pipefail
lock_dir="$1"
remote_abs="$2"
lock_owner="$3"
stale_secs="$4"
parent_dir="$(dirname "$remote_abs")"
mkdir -p "$parent_dir"

write_lock_metadata() {
    printf '%s\n' "$lock_owner" > "$lock_dir/owner"
    printf '%s\n' "$(date +%s)" > "$lock_dir/heartbeat.tmp"
    mv -f "$lock_dir/heartbeat.tmp" "$lock_dir/heartbeat"
}

read_lock_timestamp() {
    local ts=""
    if [ -f "$lock_dir/heartbeat" ]; then
        ts="$(tr -d '[:space:]' < "$lock_dir/heartbeat" 2>/dev/null || true)"
    fi
    case "$ts" in
        ''|*[!0-9]*)
            ts="$(stat -f %m "$lock_dir" 2>/dev/null || stat -c %Y "$lock_dir" 2>/dev/null || echo 0)"
            ;;
    esac
    printf '%s\n' "$ts"
}

has_active_remote_work() {
    if ps -axo comm=,args= 2>/dev/null | awk -v tgt="$remote_abs" '
        {
            comm = $1
            sub(/^.*\//, "", comm)
            if ((comm == "cargo" || comm == "rustc" || comm == "git" || comm == "rsync") && index($0, tgt) > 0) {
                found = 1
                exit 0
            }
        }
        END { exit(found ? 0 : 1) }
    '; then
        return 0
    fi
    if command -v lsof >/dev/null 2>&1; then
        if lsof -u "$(id -un)" -d cwd -F cn 2>/dev/null | awk -v tgt="$remote_abs" '
            /^p/ { cmd = "" }
            /^c/ { cmd = substr($0, 2) }
            /^n/ {
                cwd = substr($0, 2)
                if ((cmd == "cargo" || cmd == "rustc" || cmd == "git" || cmd == "rsync") &&
                    (cwd == tgt || index(cwd, tgt "/") == 1)) {
                    found = 1
                    exit 0
                }
            }
            END { exit(found ? 0 : 1) }
        '; then
            return 0
        fi
    fi
    return 1
}

waited=0
while ! mkdir "$lock_dir" 2>/dev/null; do
    if [ -d "$lock_dir" ]; then
        now="$(date +%s)"
        hb="$(read_lock_timestamp)"
        age=$(( now - hb ))
        if [ "$age" -ge "$stale_secs" ] && ! has_active_remote_work; then
            reclaim_lock="${lock_dir}.reclaim"
            if [ -d "$reclaim_lock" ]; then
                reclaim_ts="$(stat -f %m "$reclaim_lock" 2>/dev/null || stat -c %Y "$reclaim_lock" 2>/dev/null || echo 0)"
                if [ $(( now - reclaim_ts )) -ge 60 ]; then
                    rmdir "$reclaim_lock" 2>/dev/null || true
                fi
            fi
            if mkdir "$reclaim_lock" 2>/dev/null; then
                reclaimed=0
                if [ -d "$lock_dir" ]; then
                    now="$(date +%s)"
                    hb="$(read_lock_timestamp)"
                    age=$(( now - hb ))
                    if [ "$age" -ge "$stale_secs" ] && ! has_active_remote_work; then
                        owner_info="$(cat "$lock_dir/owner" 2>/dev/null || printf 'unknown')"
                        echo "warning: reclaiming stale remote macOS test lock '$lock_dir' (owner='$owner_info', age=${age}s >= ${stale_secs}s, no active cargo/rustc/git/rsync referencing '$remote_abs')" >&2
                        rm -rf "$lock_dir"
                        if mkdir "$lock_dir" 2>/dev/null; then
                            reclaimed=1
                        fi
                    fi
                else
                    if mkdir "$lock_dir" 2>/dev/null; then
                        reclaimed=1
                    fi
                fi
                rmdir "$reclaim_lock" 2>/dev/null || true
                if [ "$reclaimed" -eq 1 ]; then
                    break
                fi
            fi
        fi
    fi

    waited=$((waited + 2))
    if [ "$waited" -ge 1800 ]; then
        echo "error: timed out waiting for remote macOS test lock '$lock_dir'" >&2
        exit 1
    fi
    sleep 2
done

if ! write_lock_metadata; then
    rm -rf "$lock_dir" 2>/dev/null || true
    echo "error: failed to write metadata in remote macOS test lock '$lock_dir'" >&2
    exit 1
fi

if [ ! -d "$remote_abs/.git" ]; then
    if [ -d "$HOME/contextunity-acdd/.git" ]; then
        git clone --no-hardlinks "$HOME/contextunity-acdd" "$remote_abs"
    else
        git init -b main "$remote_abs"
    fi
fi
if git -C "$remote_abs" rev-parse --verify HEAD >/dev/null 2>&1; then
    git -C "$remote_abs" checkout -q --detach HEAD
    git -C "$remote_abs" reset -q --hard
    git -C "$remote_abs" clean -q -fd
fi
EOF
LOCK_ACQUIRED=1

heartbeat_interval=$(( STALE_SECS / 4 ))
if [ "$heartbeat_interval" -lt 1 ]; then
    heartbeat_interval=1
elif [ "$heartbeat_interval" -gt 20 ]; then
    heartbeat_interval=20
fi
printf -v remote_hb_cmd 'if [ -d %q ] && [ "$(cat %q 2>/dev/null || true)" = %q ]; then printf "%%s\\n" "$(date +%%s)" > %q && mv -f %q %q; fi' \
    "$LOCK_DIR" "$LOCK_DIR/owner" "$LOCK_OWNER" "$LOCK_DIR/heartbeat.tmp" "$LOCK_DIR/heartbeat.tmp" "$LOCK_DIR/heartbeat"
(
    trap - EXIT INT TERM
    while kill -0 "$PARENT_PID" 2>/dev/null; do
        sleep "$heartbeat_interval"
        kill -0 "$PARENT_PID" 2>/dev/null || exit 0
        ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$SSH_HOST" "$remote_hb_cmd" >/dev/null 2>&1 || true
    done
) &
HEARTBEAT_PID=$!

# Push local HEAD objects cleanly (works from primary checkout or linked `.worktrees/<run>`),
# reset remote working tree before overlaying local uncommitted edits via `rsync`.
git push -q -f "${SSH_HOST}:${REMOTE_ABS}" +HEAD:refs/heads/ci-target

ssh -T -o BatchMode=yes "$SSH_HOST" bash -s -- "$REMOTE_ABS" <<'EOF'
set -euo pipefail
cd "$1"
git checkout -q -f ci-target
git clean -q -fd
EOF

# Content-checksum sync without preserving cross-branch mtimes so older Linux worktree
# timestamps never look fresher/older incorrectly against warm remote `target/` fingerprints.
rsync -rlpcDz --delete \
    --exclude '/.git' \
    --exclude '/target' \
    --exclude '/benchmarks/target' \
    --exclude '/.worktrees' \
    --exclude '/docs/memories' \
    --exclude '/.acdd/run' \
    --exclude '/.acdd/tmp' \
    --exclude '/.acdd/reports' \
    --exclude '*.sqlite*' \
    --exclude '*.jsonl' \
    --exclude '*.log' \
    --exclude '*.lock' \
    ./ "${SSH_HOST}:${REMOTE_ABS}/"

{
    printf 'remote_abs=%q\n' "$REMOTE_ABS"
    printf 'no_clippy=%q\n' "$NO_CLIPPY"
    printf 'exact=%q\n' "$EXACT"
    printf 'filter=%q\n' "$FILTER"
    printf 'targets=()\n'
    for t in "${TARGETS[@]}"; do
        printf 'targets+=(%q)\n' "$t"
    done
    printf 'extra_args=()\n'
    for e in "${EXTRA_TEST_ARGS[@]+"${EXTRA_TEST_ARGS[@]}"}"; do
        printf 'extra_args+=(%q)\n' "$e"
    done
    cat <<'EOF'
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

cd "$remote_abs"

tmp_check_dir="${TMPDIR:-/tmp}"
tmp_check_dir="${tmp_check_dir%/}"
before_tmp=$(find /tmp "$tmp_check_dir" -maxdepth 1 -name "contextunity-test-*" 2>/dev/null | wc -l | tr -d " ")

set +e
rc=0
for tgt in "${targets[@]}"; do
    # Split intentional multi-flag target specification such as `--package acdd-state --lib`.
    read -r -a tgt_words <<< "$tgt"
    if [ "$no_clippy" -eq 0 ]; then
        echo "+ cargo clippy ${tgt_words[*]} --all-features -- -D warnings"
        cargo clippy "${tgt_words[@]}" --all-features -- -D warnings
        rc=$?
        if [ "$rc" -ne 0 ]; then
            break
        fi
    fi

    test_cmd=(cargo test "${tgt_words[@]}")
    if [ -n "$filter" ]; then
        test_cmd+=("$filter")
    fi
    if [ "$exact" -eq 1 ] || [ "${#extra_args[@]}" -gt 0 ]; then
        test_cmd+=(--)
        if [ "$exact" -eq 1 ]; then
            test_cmd+=(--exact)
        fi
        if [ "${#extra_args[@]}" -gt 0 ]; then
            test_cmd+=("${extra_args[@]}")
        fi
    fi
    echo "+ ${test_cmd[*]}"
    "${test_cmd[@]}"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        break
    fi
done
set -e

after_tmp=$(find /tmp "$tmp_check_dir" -maxdepth 1 -name "contextunity-test-*" 2>/dev/null | wc -l | tr -d " ")
echo "macOS verification complete: exit=$rc tmp_leaks_before=$before_tmp tmp_leaks_after=$after_tmp"
if [ "$rc" -ne 0 ]; then
    exit "$rc"
fi
if [ "$after_tmp" -gt "$before_tmp" ]; then
    echo "error: leaked contextunity-test-* directories on macOS ($before_tmp -> $after_tmp)" >&2
    exit 1
fi
EOF
} | ssh -o BatchMode=yes "$SSH_HOST" bash -s
