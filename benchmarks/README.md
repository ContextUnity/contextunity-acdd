# ContextUnity ACDD Benchmarks (`v0.6.2`)

Self-contained benchmark suite for measuring ContextUnity ACDD cold builds, incremental delta updates, and stdio MCP tool latency/memory against `target/release/acdd`.

## Prerequisites

1. Verify the host power profile is not throttled:
   ```sh
   powerprofilesctl get
   ```
   Set `powerprofilesctl set performance` before collecting timing baselines.
2. Build the release binary:
   ```sh
   cargo build --release
   ```

## Full Suite Execution

Run all suites (`cold`, `delta`, `mcp`) against the primary control profile (`commerce-release-update.json`):

```sh
python3 benchmarks/run_benchmarks.py \
  --profile benchmarks/profiles/commerce-release-update.json \
  --workspace /home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update \
  --binary target/release/acdd \
  --suite all \
  --repeats 5 \
  --output benchmarks/results/v062-commerce-release-update.json
```

This writes both `benchmarks/results/v062-commerce-release-update.json` and a companion Markdown summary at `benchmarks/results/v062-commerce-release-update.md`. An optional self-hosted profile is also available at `benchmarks/profiles/self-acdd.json`.

## Individual Runners

- **Cold Build** (`cold_build_benchmark.py`):
  Measures isolated `acdd --format json build` wall time (median/p95/mean ms), CPU seconds, peak RSS MiB, DB size MiB, graph counts, resolution coverage, and deterministic `output_root` across repetitions. Pass `--baseline-binary <path>` for paired A/B comparisons.
  ```sh
  python3 benchmarks/cold_build_benchmark.py \
    --repo . \
    --binary target/release/acdd \
    --repeats 5
  ```

- **Incremental Delta** (`delta_benchmark.py`):
  Clones Git-tracked workspace files into a disposable `TemporaryDirectory`, builds a warm index, and measures `acdd --format json delta` for:
  1. 1 edited source file (`edit_1_py` / `edit_1_rs`)
  2. 10 edited source files (`edit_10_py` / `edit_10_rs`)
  3. 1 edited `.md` doc file (`edit_1_md`)
  ```sh
  python3 benchmarks/delta_benchmark.py \
    --repo /path/to/workspace \
    --binary target/release/acdd \
    --repeats 5
  ```

- **MCP Latency & `inotify` Probe** (`mcp_latency_benchmark.py`):
  Spawns `target/release/acdd serve` over stdio JSON-RPC 2.0 against a disposable index DB, probes `acdd.yaml` `IN_OPEN` events on warm calls via Linux `inotify`, and measures `initialize` latency plus per-scenario p50/p95/mean ms, payload bytes, and peak RSS across all profile scenarios.
  ```sh
  python3 benchmarks/mcp_latency_benchmark.py \
    --profile benchmarks/profiles/commerce-release-update.json \
    --binary target/release/acdd \
    --repeats 5
  ```

## Isolation & Cleanup

All runners place temporary SQLite databases, extraction caches, and workspace clones inside `tempfile.TemporaryDirectory` contexts that are cleaned up automatically on exit, leaving the source repository and `.acdd/` untouched.
