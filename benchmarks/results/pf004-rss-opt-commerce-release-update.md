# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-21T16:17:21.998015+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/PF-004/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 12593.38 |
| Wall p95 (ms) | 17526.24 |
| Wall mean (ms) | 13611.62 |
| CPU median (s) | 49.698 |
| Peak RSS median (MiB) | 885.80 |
| DB size (MiB) | 199.53 |
| Files | 4150 |
| Nodes | 72293 |
| Edges | 221662 |
| Doc sections | 2967 |
| Resolution (%) | 73.3758% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `14754.56 ms` (3961 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 3709.20 | 5633.11 | 4104.02 | 3.834 | 541.81 |
| `edit_10_py` | 10 | 10 | 3273.39 | 3471.97 | 3325.40 | 3.514 | 541.81 |
| `edit_1_md` | 1 | 1 | 2964.18 | 3028.55 | 2966.50 | 3.136 | 541.81 |

## MCP Latency (`acdd serve`)

- Initialize latency: `13.52 ms`
- Peak RSS: `362.40 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 2215.98 | 2346.30 | 2241.14 | 17789 | 92.25 |
| `symbol_search` | `code_map_search` | 16.32 | 18.33 | 16.62 | 3595 | 101.61 |
| `symbol_inspection` | `code_map_inspect` | 14.88 | 18.08 | 15.13 | 5542 | 105.64 |
| `source_snippet` | `acdd_read` | 105.92 | 194.48 | 128.73 | 5957 | 151.81 |
| `test_relationships` | `code_map_tests` | 12.42 | 12.80 | 12.46 | 31918 | 156.43 |
| `unresolved_evidence` | `code_map_analyze` | 346.78 | 359.65 | 349.10 | 21131 | 182.26 |
| `impact_analysis` | `code_map_impact` | 24.50 | 25.23 | 24.56 | 26514 | 356.76 |
| `symbol_explanation` | `code_map_explain` | 12.72 | 21.70 | 14.87 | 27774 | 182.81 |
| `graph_slice_query` | `code_map_query` | 12.23 | 12.84 | 12.18 | 20166 | 181.72 |
| `workspace_diagnostics` | `code_map_analyze` | 428.90 | 459.24 | 426.96 | 22550 | 358.11 |
| `path_diagnostics_coverage` | `code_map_analyze` | 222.68 | 227.90 | 222.53 | 13226 | 358.54 |
| `removal_safety` | `code_map_prove_removal` | 32.65 | 33.97 | 32.53 | 9564 | 183.47 |
| `ast_pattern_search` | `ast_grep_search` | 13.88 | 14.53 | 13.84 | 5466 | 183.78 |
| `documentation_search` | `search_docs` | 15.32 | 22.14 | 16.56 | 4217 | 185.65 |
| `documentation_read` | `get_doc` | 63.59 | 68.53 | 64.38 | 33747 | 190.97 |
| `tool_guide` | `acdd_guide` | 9.06 | 10.81 | 9.49 | 2428 | 189.94 |
| `duplicate_detection` | `code_map_query` | 286.18 | 336.41 | 296.34 | 9693 | 292.28 |
| `dead_tail_detection` | `code_map_query` | 492.88 | 576.40 | 490.31 | 31912 | 362.40 |
