#!/usr/bin/env bash
set -euo pipefail

# Run fast repository boundary/compile/docs checks sequentially, then execute the
# full Linux workspace test suite and remote macOS SSH verification in parallel.
# Intended for `.acdd/profile.yaml`'s `check_full_repo` (`run_tests`) command.

linux_pid=""

cleanup() {
    if [ -n "$linux_pid" ] && kill -0 "$linux_pid" 2>/dev/null; then
        kill "$linux_pid" 2>/dev/null || true
        wait "$linux_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Phase 1: fast sequential fail-closed gates before spinning up full suites / SSH.
cargo test --test commitment_integrity
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -q -- docs check
cargo run -q -- validate

# Phase 2: run Linux full workspace tests (streaming to stdout) concurrently
# with remote macOS SSH verification (captured in memory via anonymous pipe).
cargo test --workspace --all-targets &
linux_pid=$!

macos_rc=0
macos_out="$(./scripts/test-macos-ssh.sh 2>&1)" || macos_rc=$?

linux_rc=0
wait "$linux_pid" || linux_rc=$?
linux_pid=""

echo "=== macOS SSH Verification (exit=$macos_rc) ==="
if [ -n "$macos_out" ]; then
    printf '%s\n' "$macos_out"
fi

if [ "$linux_rc" -ne 0 ] || [ "$macos_rc" -ne 0 ]; then
    echo "error: full repo verification failed (linux_rc=$linux_rc, macos_rc=$macos_rc)" >&2
    exit 1
fi
