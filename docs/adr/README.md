---
title: "Architectural Decisions"
doc_type: guide
---

# Architectural Decisions

Accepted decisions live here as owner-approved ADR files with stable IDs, governing invariants, and links to affected architecture and code contracts. Consolidated `0.6.0` decisions are organized into three groups:

## Group I: System Architecture, Layering & State Engine

1. [ADR 0001: Crate DAG and Layer Boundaries](0001-crate-dag-and-layer-boundaries.md)
2. [ADR 0002: Workspace Confinement and Path Policy](0002-workspace-confinement-and-path-policy.md)
3. [ADR 0003: Unified Profile Graph and State Machine](0003-unified-profile-graph-and-state-machine.md)
4. [ADR 0004: DomainProvider SPI and Plugin Tools](0004-domain-provider-spi-and-plugin-tools.md)
5. [ADR 0005: Content-Addressed Git Snapshots and Integration](0005-content-addressed-git-snapshots-and-integration.md)
6. [ADR 0006: Budgeted Gate Context and Dual-Surface Parity](0006-budgeted-gate-context-and-dual-surface-parity.md)

## Group II: Indexing, Storage & Concurrency

7. [ADR 0007: SQLite Code-Map Concurrency and Staged Publication](0007-sqlite-code-map-concurrency-and-staged-publication.md)
8. [ADR 0008: Merkle Commitments and Compressed Fact Storage](0008-merkle-commitments-and-compressed-fact-storage.md)
9. [ADR 0009: Incremental Delta and Transitive Relinking](0009-incremental-delta-and-transitive-relinking.md)

## Group III: Language Semantics, Resolution & Safety

10. [ADR 0010: Static Value Flow and Receiver Resolution](0010-static-value-flow-and-receiver-resolution.md)
11. [ADR 0011: Language Linkers and Framework Manifests](0011-language-linkers-and-framework-manifests.md)
12. [ADR 0012: Fail-Closed Selectors and Read-Only SQL Sandbox](0012-fail-closed-selectors-and-readonly-sql-sandbox.md)

Decision amendments require explicit architectural admission. See [Architecture](../architecture/README.md) for living component topology and extension guides.
