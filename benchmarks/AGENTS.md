# Benchmarks & Performance Profiling — Agent Instructions

## Mandatory Invariant: User Consent Required

> [!CAUTION]
> **Explicit User Approval Required**: Benchmark scripts run full cold builds, incremental delta rebuilds, and stdio MCP workloads.
> **DO NOT** execute benchmarks autonomously, in the background, or during normal development tasks without explicit user consent and instructions.

## Host Performance Profile Precondition

> [!IMPORTANT]
> **Performance Governor Required**: Benchmark measurements are invalid if run under the OS `power-saver` platform profile, which caps CPU clocks and inflates measured latencies.
> Before running benchmarks, verify the active host profile:
> ```bash
> powerprofilesctl get  # must be 'performance' (or at minimum not 'power-saver')
> # If power-saver: powerprofilesctl set performance
> ```
> `benchmarks/run_benchmarks.py` fails closed when `powerprofilesctl get` reports `power-saver` unless `--allow-power-saver` is explicitly passed.

## Profile-Based Benchmarks

Declarative JSON profiles live in `benchmarks/profiles/` (`commerce-release-update.json` primary control profile; `self-acdd.json` optional self-hosted profile):

- **Main Runner**:
  ```bash
  python3 benchmarks/run_benchmarks.py \
    --profile benchmarks/profiles/commerce-release-update.json \
    --workspace /home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update \
    --binary target/release/acdd \
    --suite all \
    --repeats 5 \
    --output benchmarks/results/v062-commerce-release-update.json
  ```
- **Options**:
  - `--profile <path>`: Path to benchmark JSON profile (default: `benchmarks/profiles/commerce-release-update.json`).
  - `--workspace <path>`: Optional workspace root override.
  - `--binary <path>`: Path or command name for the `acdd` release binary.
  - `--baseline-binary <path>`: Optional baseline `acdd` binary for paired cold-build comparison.
  - `--suite {all,cold,delta,mcp}`: Suite(s) to run sequentially (default: `all`).
  - `--repeats <N>`: Repetitions per measurement (default: `5`).
  - `--output <path>`: Destination JSON report path (also writes a companion `.md` Markdown summary).
  - `--allow-power-saver`: Override the `power-saver` guard (not recommended for release baselines).

## Subsystem Scripts

- `benchmarks/run_benchmarks.py`: Primary CLI orchestrator enforcing host power-profile preconditions and writing JSON + Markdown reports.
- `benchmarks/benchmark_profile.py`: Profile loader, binary resolver, tracked workspace cloning, and `wait4` CLI timing helpers.
- `benchmarks/cold_build_benchmark.py` + `benchmarks/cold_coverage_metrics.py`: Sequential isolated cold `acdd --format json build` benchmark (single-binary or paired `--baseline-binary`), verifying deterministic `output_root`, DB size, peak RSS, CPU seconds, graph counts, and resolution coverage in disposable `TemporaryDirectory` workspaces.
- `benchmarks/delta_benchmark.py`: Clones tracked workspace source files into an isolated `TemporaryDirectory`, builds a warm index, and benchmarks `acdd --format json delta` on 1 `.rs` edit, 10 `.rs` edits, and 1 `.md` edit without modifying the source tree.
- `benchmarks/mcp_latency_benchmark.py`: Spawns `acdd serve` over stdio JSON-RPC 2.0, runs all profile scenarios, verifies zero JSON-RPC/tool errors, measures initialize latency + per-scenario p50/p95/mean ms + payload bytes + peak RSS, and probes `acdd.yaml` open counts on warm calls via Linux `inotify`.

## Staged Lifecycle

When explicitly instructed to run benchmarks:
1. Verify host power profile: `powerprofilesctl get` (ensure `performance`).
2. Compile release binary: `cargo build --release`.
3. Execute controlled run: `python3 benchmarks/run_benchmarks.py --profile benchmarks/profiles/commerce-release-update.json --workspace /home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update --binary target/release/acdd --suite all --repeats 5 --output benchmarks/results/v062-commerce-release-update.json`.
4. Verify no temporary directories leaked under `/tmp/` and zero changes in the target control workspace.
