#!/usr/bin/env bash
set -euo pipefail

# Run fast repository boundary/compile/docs checks sequentially, then execute the
# full Linux workspace test suite and remote macOS SSH verification in parallel.
# Intended for `.acdd/workflows/default.yaml`'s `check_full_repo` (`run_tests`) command.

linux_pid=""

terminate_tree() {
    local pid="$1"
    local sig="${2:-TERM}"
    local children=""
    local child=""
    if command -v pgrep >/dev/null 2>&1; then
        children="$(pgrep -P "$pid" 2>/dev/null || true)"
    else
        children="$(ps -eo pid=,ppid= 2>/dev/null | awk -v p="$pid" '$2 == p {print $1}' || true)"
    fi
    for child in $children; do
        terminate_tree "$child" "$sig"
    done
    kill "-$sig" "$pid" 2>/dev/null || true
}

stop_process_tree() {
    local pid="$1"
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        return 0
    fi
    terminate_tree "$pid" TERM
    local tries=0
    while [ "$tries" -lt 10 ] && kill -0 "$pid" 2>/dev/null; do
        sleep 0.2
        tries=$((tries + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        terminate_tree "$pid" KILL
    fi
    wait "$pid" 2>/dev/null || true
}

cleanup() {
    local exit_code=$?
    trap - EXIT INT TERM
    if [ -n "$linux_pid" ]; then
        stop_process_tree "$linux_pid"
        linux_pid=""
    fi
    local children=""
    local child=""
    if command -v pgrep >/dev/null 2>&1; then
        children="$(pgrep -P "$$" 2>/dev/null || true)"
    else
        children="$(ps -eo pid=,ppid= 2>/dev/null | awk -v p="$$" '$2 == p {print $1}' || true)"
    fi
    for child in $children; do
        stop_process_tree "$child"
    done
    return "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Compute adaptive Cargo build job cap when unset (~3 GiB/job from MemAvailable, clamped [2, 4])
# to prevent rustc/clippy OOM kills on high-core hosts; honor explicit CARGO_BUILD_JOBS.
if [ -z "${CARGO_BUILD_JOBS:-}" ]; then
    mem_avail_kb="$(awk '/^MemAvailable:/ {print $2; exit}' /proc/meminfo 2>/dev/null || true)"
    case "$mem_avail_kb" in
        ''|*[!0-9]*)
            CARGO_BUILD_JOBS=2
            ;;
        *)
            CARGO_BUILD_JOBS=$(( mem_avail_kb / 3145728 ))
            if [ "$CARGO_BUILD_JOBS" -lt 2 ]; then
                CARGO_BUILD_JOBS=2
            elif [ "$CARGO_BUILD_JOBS" -gt 4 ]; then
                CARGO_BUILD_JOBS=4
            fi
            ;;
    esac
fi
export CARGO_BUILD_JOBS

full_test_threads="${ACDD_FULL_TEST_THREADS:-4}"
case "$full_test_threads" in
    ''|*[!0-9]*|0)
        echo "error: ACDD_FULL_TEST_THREADS must be a positive integer (got '$full_test_threads')" >&2
        exit 1
        ;;
esac

count_local_tmp_leaks() {
    local tmp_check_dir="${TMPDIR:-/tmp}"
    tmp_check_dir="${tmp_check_dir%/}"
    if [ -z "$tmp_check_dir" ] || [ "$tmp_check_dir" = "/tmp" ]; then
        find /tmp -maxdepth 1 -name "contextunity-test-*" 2>/dev/null | wc -l | tr -d " "
    else
        find /tmp "$tmp_check_dir" -maxdepth 1 -name "contextunity-test-*" 2>/dev/null | wc -l | tr -d " "
    fi
}

echo "Full repo verification caps: CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}, ACDD_FULL_TEST_THREADS=${full_test_threads}"
before_tmp="$(count_local_tmp_leaks)"

# Phase 1: fast sequential fail-closed gates before spinning up full suites / SSH.
cargo test --test commitment_integrity
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -q -- docs check
cargo run -q -- validate

# Phase 2: run Linux full workspace tests (streaming to stdout) concurrently
# with remote macOS SSH verification (captured in memory via anonymous pipe).
cargo test --workspace --all-targets -- --test-threads="$full_test_threads" &
linux_pid=$!

macos_rc=0
macos_out="$(./scripts/test-macos-ssh.sh 2>&1)" || macos_rc=$?

linux_rc=0
wait "$linux_pid" || linux_rc=$?
linux_pid=""

after_tmp="$(count_local_tmp_leaks)"

echo "=== macOS SSH Verification (exit=$macos_rc) ==="
if [ -n "$macos_out" ]; then
    printf '%s\n' "$macos_out"
fi
echo "Local Linux verification summary: exit=$linux_rc tmp_leaks_before=$before_tmp tmp_leaks_after=$after_tmp"

if [ "$linux_rc" -ne 0 ] || [ "$macos_rc" -ne 0 ]; then
    echo "error: full repo verification failed (linux_rc=$linux_rc, macos_rc=$macos_rc)" >&2
    exit 1
fi
if [ "$after_tmp" -ne "$before_tmp" ]; then
    echo "error: local contextunity-test-* count changed during run ($before_tmp -> $after_tmp)" >&2
    exit 1
fi
