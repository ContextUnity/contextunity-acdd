# ACDD v0.6.2 Benchmark Report (commerce-release-update)

- **Timestamp (UTC)**: `2026-11-25T12:54:35.487060+00:00`
- **Workspace**: `/home/oleksii/ContextUnity/contextunity/.worktrees/commerce-release-update`
- **Binary**: `/home/oleksii/ContextUnity/projects/contextunity-acdd/target/release/acdd`
- **Power Profile**: `performance`
- **CPU / Platform**: `16` cores — `Linux-7.3.4-arch1-2-x86_64-with-glibc2.44`
- **Repeats**: `5`

## Cold Build

| Metric | Value |
| --- | ---: |
| Wall median (ms) | 11342.94 |
| Wall p95 (ms) | 11449.75 |
| Wall mean (ms) | 11333.29 |
| CPU median (s) | 54.029 |
| Peak RSS median (MiB) | 916.56 |
| DB size (MiB) | 210.91 |
| Files | 4276 |
| Nodes | 74510 |
| Edges | 228894 |
| Doc sections | 3069 |
| Resolution (%) | 75.6079% |
| Deterministic `output_root` | `True` |

## Incremental Delta

- Warm baseline build: `11046.15 ms` (3983 tracked files)

| Scenario | Modified | Affected Owners | Wall median (ms) | Wall p95 (ms) | Wall mean (ms) | CPU median (s) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `edit_1_py` | 1 | 1 | 954.88 | 965.21 | 939.48 | 0.864 | 557.48 |
| `edit_10_py` | 10 | 10 | 1159.56 | 1169.05 | 1131.95 | 1.107 | 557.48 |
| `edit_1_md` | 1 | 1 | 1020.73 | 1027.24 | 1018.27 | 0.986 | 557.48 |

## MCP Latency (`acdd serve`)

- Initialize latency: `38.90 ms`
- Peak RSS: `415.71 MiB`
- `inotify` `acdd.yaml` opens: `warm=1`, `initial=2` (`single_load_on_warm_call=True`)
- Scenarios: `18` (total errors: `0`)

| Scenario | Tool | p50 (ms) | p95 (ms) | mean (ms) | Payload (B) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `workspace_overview` | `code_map_overview` | 1955.88 | 2033.55 | 1974.52 | 19726 | 110.89 |
| `symbol_search` | `code_map_search` | 13.12 | 14.29 | 13.31 | 3316 | 121.17 |
| `symbol_inspection` | `code_map_inspect` | 25.79 | 26.12 | 25.79 | 5482 | 128.91 |
| `source_snippet` | `acdd_read` | 90.99 | 91.76 | 91.11 | 6194 | 175.44 |
| `test_relationships` | `code_map_tests` | 12.44 | 13.41 | 12.66 | 37295 | 175.43 |
| `unresolved_evidence` | `code_map_analyze` | 240.14 | 326.78 | 262.76 | 21501 | 202.19 |
| `impact_analysis` | `code_map_impact` | 34.05 | 34.43 | 34.12 | 30858 | 202.84 |
| `symbol_explanation` | `code_map_explain` | 12.21 | 13.47 | 12.53 | 30055 | 202.82 |
| `graph_slice_query` | `code_map_query` | 11.25 | 11.63 | 11.34 | 21603 | 202.94 |
| `workspace_diagnostics` | `code_map_analyze` | 576.04 | 576.85 | 575.69 | 22920 | 202.77 |
| `path_diagnostics_coverage` | `code_map_analyze` | 119.53 | 120.39 | 119.60 | 12759 | 203.44 |
| `removal_safety` | `code_map_prove_removal` | 26.90 | 27.93 | 27.15 | 11412 | 203.42 |
| `ast_pattern_search` | `ast_grep_search` | 11.42 | 12.32 | 11.53 | 5703 | 203.65 |
| `documentation_search` | `search_docs` | 13.77 | 14.77 | 13.98 | 4168 | 205.45 |
| `documentation_read` | `get_doc` | 34.63 | 38.13 | 35.45 | 33747 | 213.44 |
| `tool_guide` | `acdd_guide` | 8.15 | 8.48 | 8.22 | 2428 | 214.01 |
| `duplicate_detection` | `code_map_query` | 115.11 | 115.83 | 115.26 | 4309 | 233.19 |
| `dead_tail_detection` | `code_map_query` | 597.12 | 656.52 | 605.93 | 30409 | 415.71 |
