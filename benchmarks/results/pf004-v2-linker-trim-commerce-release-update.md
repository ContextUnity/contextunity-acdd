# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-21T17:58:41.722019+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/PF-004/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 10022.39 |
| Wall p95 (ms) | 11189.20 |
| Wall mean (ms) | 10334.99 |
| CPU median (s) | 42.425 |
| Peak RSS median (MiB) | 816.43 |
| DB size (MiB) | 199.50 |
| Files | 4150 |
| Nodes | 72293 |
| Edges | 221662 |
| Doc sections | 2967 |
| Resolution (%) | 73.3758% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `10538.10 ms` (3961 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 2830.05 | 2933.66 | 2854.70 | 2.776 | 754.23 |
| `edit_10_py` | 10 | 10 | 3476.39 | 3529.11 | 3438.92 | 3.479 | 754.23 |
| `edit_1_md` | 1 | 1 | 3077.23 | 3179.41 | 3085.66 | 3.060 | 754.23 |

## MCP Latency (`acdd serve`)

- Initialize latency: `14.83 ms`
- Peak RSS: `369.11 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 1963.54 | 2084.63 | 2006.81 | 19299 | 92.75 |
| `symbol_search` | `code_map_search` | 14.27 | 16.63 | 14.70 | 3595 | 104.88 |
| `symbol_inspection` | `code_map_inspect` | 12.30 | 14.40 | 12.46 | 5542 | 106.07 |
| `source_snippet` | `acdd_read` | 96.09 | 100.56 | 96.03 | 5957 | 151.69 |
| `test_relationships` | `code_map_tests` | 13.70 | 14.76 | 13.79 | 31918 | 151.52 |
| `unresolved_evidence` | `code_map_analyze` | 342.09 | 459.39 | 372.11 | 21131 | 183.25 |
| `impact_analysis` | `code_map_impact` | 25.48 | 28.67 | 25.70 | 26514 | 183.56 |
| `symbol_explanation` | `code_map_explain` | 12.05 | 15.22 | 12.96 | 27774 | 358.69 |
| `graph_slice_query` | `code_map_query` | 11.23 | 13.43 | 11.68 | 20166 | 180.04 |
| `workspace_diagnostics` | `code_map_analyze` | 393.72 | 490.82 | 415.00 | 22550 | 339.04 |
| `path_diagnostics_coverage` | `code_map_analyze` | 228.92 | 306.54 | 244.25 | 13226 | 174.56 |
| `removal_safety` | `code_map_prove_removal` | 29.88 | 30.34 | 29.60 | 9564 | 176.66 |
| `ast_pattern_search` | `ast_grep_search` | 12.82 | 13.33 | 12.71 | 5466 | 177.02 |
| `documentation_search` | `search_docs` | 12.37 | 13.44 | 12.50 | 4217 | 349.75 |
| `documentation_read` | `get_doc` | 61.02 | 63.95 | 61.51 | 33747 | 369.11 |
| `tool_guide` | `acdd_guide` | 9.45 | 9.77 | 9.46 | 2428 | 184.55 |
| `duplicate_detection` | `code_map_query` | 274.46 | 317.04 | 285.51 | 9693 | 291.29 |
| `dead_tail_detection` | `code_map_query` | 439.23 | 557.23 | 463.21 | 31912 | 361.84 |
