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

cleanup_remote_lock() {
    ssh -n -o BatchMode=yes "$SSH_HOST" "rmdir '$LOCK_DIR' 2>/dev/null || true" >/dev/null 2>&1 || true
}
trap cleanup_remote_lock EXIT

# Serialize concurrent local runs sharing the same warm remote `target/` cache.
ssh -n -o BatchMode=yes "$SSH_HOST" bash -s -- "$LOCK_DIR" "$REMOTE_ABS" <<'EOF'
set -euo pipefail
lock_dir="$1"
remote_abs="$2"
parent_dir="$(dirname "$remote_abs")"
mkdir -p "$parent_dir"

waited=0
while ! mkdir "$lock_dir" 2>/dev/null; do
    waited=$((waited + 2))
    if [ "$waited" -ge 1800 ]; then
        echo "error: timed out waiting for remote macOS test lock '$lock_dir'" >&2
        exit 1
    fi
    sleep 2
done

if [ ! -d "$remote_abs/.git" ]; then
    if [ -d "$HOME/contextunity-acdd/.git" ]; then
        git clone --no-hardlinks "$HOME/contextunity-acdd" "$remote_abs"
    else
        git init -b main "$remote_abs"
    fi
fi
EOF

# Push local HEAD objects cleanly (works from primary checkout or linked `.worktrees/<run>`),
# reset remote working tree before overlaying local uncommitted edits via `rsync`.
git push -q -f "${SSH_HOST}:${REMOTE_ABS}" +HEAD:refs/heads/ci-target

ssh -n -o BatchMode=yes "$SSH_HOST" bash -s -- "$REMOTE_ABS" <<'EOF'
set -euo pipefail
cd "$1"
git checkout -q -f ci-target
git clean -q -fd
EOF

rsync -az --delete \
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
