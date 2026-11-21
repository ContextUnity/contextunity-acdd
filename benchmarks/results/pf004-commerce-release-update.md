# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-21T14:59:48.108192+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/PF-004/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 11150.74 |
| Wall p95 (ms) | 12101.16 |
| Wall mean (ms) | 11203.59 |
| CPU median (s) | 47.284 |
| Peak RSS median (MiB) | 938.25 |
| DB size (MiB) | 199.81 |
| Files | 4156 |
| Nodes | 72397 |
| Edges | 221945 |
| Doc sections | 2979 |
| Resolution (%) | 73.3872% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `10967.24 ms` (3961 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 2860.27 | 2882.76 | 2830.91 | 2.827 | 543.26 |
| `edit_10_py` | 10 | 10 | 3119.04 | 3164.37 | 3110.17 | 3.266 | 543.26 |
| `edit_1_md` | 1 | 1 | 2807.50 | 2844.39 | 2789.09 | 2.850 | 543.26 |

## MCP Latency (`acdd serve`)

- Initialize latency: `12.56 ms`
- Peak RSS: `366.50 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 2053.55 | 2071.11 | 2019.65 | 19299 | 91.70 |
| `symbol_search` | `code_map_search` | 17.24 | 21.69 | 18.31 | 3595 | 104.52 |
| `symbol_inspection` | `code_map_inspect` | 14.01 | 14.65 | 13.98 | 5542 | 105.02 |
| `source_snippet` | `acdd_read` | 98.76 | 101.57 | 98.73 | 5957 | 150.13 |
| `test_relationships` | `code_map_tests` | 14.80 | 16.15 | 14.88 | 31918 | 292.22 |
| `unresolved_evidence` | `code_map_analyze` | 325.31 | 430.60 | 351.48 | 21131 | 358.29 |
| `impact_analysis` | `code_map_impact` | 23.43 | 24.64 | 23.64 | 26514 | 183.62 |
| `symbol_explanation` | `code_map_explain` | 11.80 | 12.01 | 11.77 | 27774 | 183.65 |
| `graph_slice_query` | `code_map_query` | 11.22 | 11.70 | 11.33 | 20166 | 183.87 |
| `workspace_diagnostics` | `code_map_analyze` | 401.31 | 413.50 | 400.02 | 22550 | 181.19 |
| `path_diagnostics_coverage` | `code_map_analyze` | 206.13 | 285.10 | 225.33 | 13226 | 184.04 |
| `removal_safety` | `code_map_prove_removal` | 27.84 | 28.18 | 27.92 | 9564 | 184.30 |
| `ast_pattern_search` | `ast_grep_search` | 11.44 | 11.95 | 11.59 | 5466 | 184.69 |
| `documentation_search` | `search_docs` | 11.20 | 11.47 | 11.26 | 4217 | 364.29 |
| `documentation_read` | `get_doc` | 59.12 | 60.02 | 58.75 | 33747 | 194.29 |
| `tool_guide` | `acdd_guide` | 8.61 | 8.82 | 8.65 | 2428 | 193.75 |
| `duplicate_detection` | `code_map_query` | 262.68 | 317.16 | 275.54 | 9693 | 296.23 |
| `dead_tail_detection` | `code_map_query` | 391.70 | 403.80 | 394.31 | 31912 | 366.50 |
