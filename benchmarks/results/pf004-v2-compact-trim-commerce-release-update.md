# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-21T17:36:08.759866+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/PF-004/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 13756.92 |
| Wall p95 (ms) | 16098.12 |
| Wall mean (ms) | 13925.74 |
| CPU median (s) | 51.494 |
| Peak RSS median (MiB) | 896.34 |
| DB size (MiB) | 199.50 |
| Files | 4150 |
| Nodes | 72293 |
| Edges | 221662 |
| Doc sections | 2967 |
| Resolution (%) | 73.3758% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `21666.84 ms` (3961 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 3668.25 | 5951.99 | 4267.33 | 3.948 | 542.27 |
| `edit_10_py` | 10 | 10 | 11504.90 | 13891.37 | 11372.27 | 8.501 | 542.27 |
| `edit_1_md` | 1 | 1 | 3887.39 | 10220.36 | 5610.64 | 3.879 | 542.27 |

## MCP Latency (`acdd serve`)

- Initialize latency: `42.08 ms`
- Peak RSS: `511.70 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 2427.96 | 3424.44 | 2622.84 | 17789 | 94.14 |
| `symbol_search` | `code_map_search` | 92.88 | 97.48 | 84.80 | 3595 | 107.96 |
| `symbol_inspection` | `code_map_inspect` | 65.32 | 69.48 | 62.39 | 5542 | 108.46 |
| `source_snippet` | `acdd_read` | 304.43 | 439.32 | 337.95 | 5957 | 164.31 |
| `test_relationships` | `code_map_tests` | 21.95 | 41.27 | 27.37 | 31918 | 164.51 |
| `unresolved_evidence` | `code_map_analyze` | 894.54 | 1599.10 | 1064.69 | 21131 | 354.19 |
| `impact_analysis` | `code_map_impact` | 32.71 | 37.34 | 32.63 | 26514 | 197.89 |
| `symbol_explanation` | `code_map_explain` | 16.32 | 19.85 | 17.05 | 27774 | 193.64 |
| `graph_slice_query` | `code_map_query` | 19.02 | 28.46 | 22.04 | 20166 | 197.69 |
| `workspace_diagnostics` | `code_map_analyze` | 561.99 | 691.75 | 581.55 | 22550 | 197.65 |
| `path_diagnostics_coverage` | `code_map_analyze` | 283.98 | 396.48 | 312.06 | 13226 | 197.91 |
| `removal_safety` | `code_map_prove_removal` | 40.98 | 41.98 | 40.70 | 9564 | 194.61 |
| `ast_pattern_search` | `ast_grep_search` | 15.53 | 15.85 | 15.54 | 5466 | 193.97 |
| `documentation_search` | `search_docs` | 16.18 | 17.95 | 16.54 | 4217 | 200.02 |
| `documentation_read` | `get_doc` | 77.65 | 83.78 | 78.38 | 33747 | 207.85 |
| `tool_guide` | `acdd_guide` | 18.93 | 21.08 | 18.41 | 2428 | 208.10 |
| `duplicate_detection` | `code_map_query` | 426.29 | 606.65 | 473.61 | 9693 | 511.70 |
| `dead_tail_detection` | `code_map_query` | 460.17 | 506.51 | 464.28 | 31912 | 365.43 |
