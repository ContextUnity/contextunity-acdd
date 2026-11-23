---
title: "Verify workspace framework rules"
doc_type: guide
---

# Verify workspace framework rules

Use this procedure with the [manifest schema](../reference/framework-manifests.md)
and the actual APIs in the indexed project. A manifest supplies data for existing
operations; it does not add a parser or execute framework code.

## Extend or disable a pack

1. Copy a verified example into the workspace's `.acdd/frameworks/` directory,
   retaining the intended filename stem. Use `vue-runtime`, not `vue`, for the
   bundled Vue rules. Add only supported typed rule sections.
2. Default merging retains other bundled rules and replaces matching identities
   with local entries. Use `replace = true` only for an intentional full pack
   replacement. To disable an effective pack, add `enabled = false` to a valid
   manifest; its rules may remain nonempty. Removing that file restores
   ordinary bundled eligibility. Linked workspaces have independent overrides.
3. Refresh the index with the ordinary CLI `build` or `delta` command, or let MCP
   freshness admission refresh it. Invalid manifests abort publication. Inspect
   `framework_rule_diagnostics` in the build/update report for removed eligible
   identities; equal rule counts do not prove equivalent rules.

## Inspect the actual result

1. **Verify workspace & generation**: call `code_map_overview` to confirm the active root and index generation.
2. **Inspect consumer & provider edges**: locate both symbols with `code_map_search`, then inspect `calls`, `renders_template`, or route edges via `code_map_inspect` and `code_map_explain` (or run read-only SQL via `code_map_query`; see [MCP reference](../reference/mcp-tools.md)).
3. **Test positive & negative cases**: verify a unique static target, missing/duplicate targets, dynamic inputs, lexical shadowing, rule disable/restore, and incremental `delta` parity against a clean `build`.

## Report a real-control comparison

Retain the baseline database and bind its SHA-256 to the source inventory, adapter
and binary used to create it. Compare a candidate on the same source bytes and
configuration. Report each control repository separately with complete resolved,
ambiguous, unresolved and external counts (R/A/U/E), exact occurrence identities
and canonical edge changes. A source-context cohort can overlap another cohort;
it does not by itself attribute a change to one rule. Record rule attribution and
activation-only effects separately, and use N/A when the denominator is zero.

Keep the original raw policy exit and precision exceptions. E is not R; a local
runtime override is not proof of embedded-default behavior. In particular, an
explicit isolated-copy Alpine activation is a separate configuration effect.
Use the final bound measurement report for current results rather than recycling
an earlier iteration's numbers. Coverage observations make no timing, memory or
throughput promise, and this workflow adds no benchmark command or numeric target.
