---
doc_type: guide
title: Documentation
---

# Documentation

The [repository README](../README.md) explains how the code map, the documentation index, and the unified state engine work together, and where an agent receives a bounded slice of context. These pages are the contracts behind that explanation. Read [documentation instructions](AGENTS.md) before editing them.

If you are connecting a client, start with [MCP setup](reference/mcp-setup.md): install the binary, point it at the workspace, and check the first index. The adapter fields behind that setup — roots, linked repositories, response size — are in [configuration](reference/configuration.md).

If you are planning work, read [ACDD](reference/acdd.md) for the loop, [state & task operations](reference/tasks.md) for runs, claims, verification, and receipts, and [ACDD profiles](reference/profiles.md) for v2 gate topologies, `git:` policies, roles, and `DomainProvider` capabilities. Admitted work lives in [milestones](milestones/README.md). Proposals that are not yet admitted live in [plans](plans/README.md). [Roadmap](../ROADMAP.md) is the short strategic note above both.

If you are writing pages ACDD should retrieve, follow [documentation instructions](AGENTS.md). Headings become searchable sections. A backticked code claim is a candidate for linking; only a unique eligible declaration in the first non-empty match tier becomes a verified edge. Use an exact declaration name and, when needed, a source-file link to confine resolution. ACDD keeps ambiguous candidates for navigation without treating them as verified coverage. See the [documentation query contract](reference/mcp-tools.md#documentation-queries).

If you are reading or extending the graph, the [MCP tools](reference/mcp-tools.md) reference and the [CLI](reference/cli.md) reference name every call. [Languages](reference/languages.md) lists compiled profiles and the static limits of each one. [Framework manifests](reference/framework-manifests.md) explains project rule files and what it takes to add a language. [Indexing](architecture/indexing.md) and [engine modularity](architecture/modularity.md) describe how extraction, linking, and storage fit together.

If you are installing skills, the [skills index](../skills/README.md) lists `acdd` and the companion skills, and what to put in `AGENTS.md`.

Operational ceilings, freshness, and what a failed query looks like are in [limits and freshness](runbooks/limits-and-freshness.md). Verification commands are in [TESTS.md](../TESTS.md) and [tests/AGENTS.md](../tests/AGENTS.md).

## Reference

- [MCP setup](reference/mcp-setup.md)
- [CLI](reference/cli.md)
- [MCP tools](reference/mcp-tools.md)
- [Configuration](reference/configuration.md)
- [ACDD](reference/acdd.md)
- [ACDD profiles](reference/profiles.md)
- [State & task operations](reference/tasks.md)
- [Languages](reference/languages.md)
- [Framework manifests](reference/framework-manifests.md)

## Architecture

- [Architecture index](architecture/README.md)
- [Indexing and resolution](architecture/indexing.md)
- [SQLite concurrency](architecture/concurrency.md)
- [Engine modularity](architecture/modularity.md)
- [Performance guidance](architecture/performance.md)
- [Architectural decisions](adr/README.md)

## Direction

- [Roadmap](../ROADMAP.md)
- [Plans](plans/README.md)
- [Milestones](milestones/README.md)
- [Historical archive](archive/README.md)

Markdown and MDX in configured documentation roots are searchable, including individual milestone and plan contracts. Their inclusion does not admit a proposal as a task or change which manifests own lifecycle and execution authority; see [configuration](reference/configuration.md#indexing-scope) and [milestones](milestones/README.md).

For a code change, start with the workspace overview, select an exact symbol, inspect direct relationships, and verify the relevant source. Missing edges are bounded by the indexed languages, roots, and unresolved-reference coverage.
