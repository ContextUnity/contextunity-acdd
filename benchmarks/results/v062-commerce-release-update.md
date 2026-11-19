# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-18T17:36:42.076590+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/.worktrees/v062-w6-benchmarks/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 12256.98 |
| Wall p95 (ms) | 12550.49 |
| Wall mean (ms) | 12210.06 |
| CPU median (s) | 44.749 |
| Peak RSS median (MiB) | 682.62 |
| DB size (MiB) | 190.81 |
| Files | 4088 |
| Nodes | 69948 |
| Edges | 214857 |
| Doc sections | 2874 |
| Resolution (%) | 74.7046% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `12000.92 ms` (3898 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 5416.10 | 5711.45 | 5497.37 | 5.591 | 522.57 |
| `edit_10_py` | 10 | 10 | 6146.31 | 6411.66 | 6155.87 | 6.422 | 522.57 |
| `edit_1_md` | 1 | 1 | 5550.92 | 5642.41 | 5559.13 | 5.600 | 522.57 |

## MCP Latency (`acdd serve`)

- Initialize latency: `13.33 ms`
- Peak RSS: `307.75 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 1940.10 | 2100.49 | 1973.08 | 19298 | 89.26 |
| `symbol_search` | `code_map_search` | 8.29 | 8.52 | 8.29 | 3573 | 96.13 |
| `symbol_inspection` | `code_map_inspect` | 5.50 | 5.87 | 5.50 | 5023 | 96.89 |
| `source_snippet` | `acdd_read` | 82.58 | 87.22 | 83.58 | 5722 | 139.95 |
| `test_relationships` | `code_map_tests` | 6.62 | 6.81 | 6.67 | 25790 | 140.12 |
| `unresolved_evidence` | `code_map_analyze` | 320.32 | 384.08 | 339.43 | 21131 | 166.80 |
| `impact_analysis` | `code_map_impact` | 22.60 | 35.27 | 25.87 | 21209 | 167.24 |
| `symbol_explanation` | `code_map_explain` | 6.65 | 7.02 | 6.72 | 24448 | 167.24 |
| `graph_slice_query` | `code_map_query` | 6.09 | 6.45 | 6.16 | 20156 | 167.24 |
| `workspace_diagnostics` | `code_map_analyze` | 374.35 | 412.24 | 382.54 | 22550 | 169.37 |
| `path_diagnostics_coverage` | `code_map_analyze` | 204.26 | 209.69 | 205.25 | 11244 | 169.37 |
| `removal_safety` | `code_map_prove_removal` | 22.62 | 22.99 | 22.35 | 7583 | 169.50 |
| `ast_pattern_search` | `ast_grep_search` | 6.80 | 7.62 | 6.81 | 5139 | 169.88 |
| `documentation_search` | `search_docs` | 6.26 | 6.41 | 6.29 | 7376 | 171.25 |
| `documentation_read` | `get_doc` | 50.72 | 51.34 | 50.59 | 33747 | 179.46 |
| `tool_guide` | `acdd_guide` | 4.03 | 4.77 | 4.09 | 2416 | 179.46 |
| `duplicate_detection` | `code_map_query` | 395.98 | 399.50 | 392.78 | 22769 | 199.97 |
| `dead_tail_detection` | `code_map_query` | 420.83 | 496.78 | 439.32 | 95100 | 307.75 |
