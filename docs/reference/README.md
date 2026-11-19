---
title: "Reference index"
doc_type: guide
---

# Reference

- [Configuration](configuration.md): `acdd.yaml`, `specs:`, `state_db`, `modules:`, and `adapter.digest` isolation.
- [CLI](cli.md): flat `0.6.1` CLI commands and global options.
- [MCP tools](mcp-tools.md): 19 built-in tools, `<domain>_<name>` plugin tools, selectors, and pagination.
- [Markdown and JSON presentation](mcp-markdown-contracts.md): format selection, domain layouts, evidence preservation, and continuations.
- [ACDD](acdd.md): unified `Run` / `GateProfile` lifecycle, `ClaimResp`, and embedded profile walkthrough.
- [ACDD profiles](profiles.md): v2 `GateProfile` schema, `git:`/`save:` policies, `DomainProvider` hooks, and pins.
- [State, tasks & lifecycle](tasks.md): `crates/acdd-state` + `crates/acdd-core`, SQLite schema, locks, candidate verification, and error codes.
- [MCP setup](mcp-setup.md): client installation and workspace selection.
- [Languages](languages.md): compiled profiles and extraction coverage.
- [Framework manifests](framework-manifests.md): bundled rules, project manifest files, validation, and language-extension boundaries.
- [HTMX and Alpine](framework-manifests-htmx-alpine.md), [Nuxt, Vue and Pinia](framework-manifests-nuxt-vue-pinia.md), and [GridViewSpec](framework-manifests-gridviewspec.md): supported static pack behavior and boundaries.
- [Workspace framework verification](../runbooks/framework-packs.md): extend, disable and inspect effective rules.

Verify interface changes against the owning Rust implementation before updating
these contracts.
