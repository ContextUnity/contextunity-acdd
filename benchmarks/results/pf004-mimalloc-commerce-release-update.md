# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-21T16:42:15.290183+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/PF-004/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 10350.18 |
| Wall p95 (ms) | 11076.39 |
| Wall mean (ms) | 10525.90 |
| CPU median (s) | 46.713 |
| Peak RSS median (MiB) | 1033.48 |
| DB size (MiB) | 199.53 |
| Files | 4150 |
| Nodes | 72293 |
| Edges | 221662 |
| Doc sections | 2967 |
| Resolution (%) | 73.3758% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `11026.37 ms` (3961 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 2756.42 | 2831.43 | 2769.38 | 2.730 | 754.51 |
| `edit_10_py` | 10 | 10 | 2922.74 | 3173.41 | 2990.22 | 3.117 | 754.51 |
| `edit_1_md` | 1 | 1 | 2907.17 | 3021.59 | 2931.33 | 3.032 | 754.51 |

## MCP Latency (`acdd serve`)

- Initialize latency: `68.51 ms`
- Peak RSS: `420.00 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 2038.76 | 2124.44 | 2031.65 | 19299 | 124.94 |
| `symbol_search` | `code_map_search` | 13.33 | 15.99 | 14.03 | 3595 | 126.35 |
| `symbol_inspection` | `code_map_inspect` | 10.86 | 13.12 | 11.51 | 5542 | 127.41 |
| `source_snippet` | `acdd_read` | 95.80 | 97.90 | 96.07 | 5957 | 172.32 |
| `test_relationships` | `code_map_tests` | 13.91 | 16.95 | 14.47 | 31918 | 172.13 |
| `unresolved_evidence` | `code_map_analyze` | 333.82 | 429.60 | 357.43 | 21131 | 207.43 |
| `impact_analysis` | `code_map_impact` | 24.78 | 25.77 | 24.91 | 26514 | 199.01 |
| `symbol_explanation` | `code_map_explain` | 11.36 | 14.84 | 12.64 | 27774 | 201.48 |
| `graph_slice_query` | `code_map_query` | 10.65 | 10.84 | 10.60 | 20166 | 201.57 |
| `workspace_diagnostics` | `code_map_analyze` | 387.30 | 398.34 | 387.33 | 22550 | 217.09 |
| `path_diagnostics_coverage` | `code_map_analyze` | 215.99 | 278.44 | 230.51 | 13226 | 210.68 |
| `removal_safety` | `code_map_prove_removal` | 28.65 | 32.88 | 30.14 | 9564 | 203.78 |
| `ast_pattern_search` | `ast_grep_search` | 12.30 | 14.20 | 12.42 | 5466 | 400.94 |
| `documentation_search` | `search_docs` | 10.71 | 10.97 | 10.74 | 4217 | 206.62 |
| `documentation_read` | `get_doc` | 60.84 | 67.30 | 61.66 | 33747 | 420.00 |
| `tool_guide` | `acdd_guide` | 11.69 | 11.92 | 11.46 | 2428 | 214.29 |
| `duplicate_detection` | `code_map_query` | 241.70 | 257.01 | 246.05 | 9693 | 289.36 |
| `dead_tail_detection` | `code_map_query` | 377.08 | 416.41 | 390.78 | 31912 | 323.65 |
