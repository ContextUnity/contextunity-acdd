# Hierarchical ACDD Profile Catalog & Domain Examples

This catalog provides domain-tailored `version: 2` ACDD profiles, lifecycle presets, and scenario examples implementing [ADR 0003](../../../../docs/adr/0003-unified-profile-graph-and-state-machine.md).

---

## 1. Scenario Examples (`{scenario}.{workflow|plan|milestone}.yaml`)

| Scenario File | Lifecycle & Inheritance | Purpose |
| :--- | :--- | :--- |
| **[`pentest-security-audit.workflow.yaml`](pentest-security-audit.workflow.yaml)** | `Workflow` · `inherit: none` · `save: none` | Self-contained penetration & security audit pipeline (recon, exploit probe, remediation, independent signoff) with zero `.acdd/profile.yaml` dependency. |
| **[`ci-verification-gate.workflow.yaml`](ci-verification-gate.workflow.yaml)** | `Workflow` · `inherit: none` · `save: none` | Clean-room standalone CI verification & autonomous repair loop for release/PR gates. |
| **[`repo-polish.workflow.yaml`](repo-polish.workflow.yaml)** | `Workflow` · `inherit: none` · `save: none` | Portable stateless polish workflow with explicit roles and commands. |
| **[`docs-refactor-and-reconcile.workflow.yaml`](docs-refactor-and-reconcile.workflow.yaml)** | `Workflow` · `inherit: none` · `save: none` | Refactors and synchronizes docs, `AGENTS.md`, skills, and examples (`audit_drift` $\to$ `reconcile_docs` $\to$ `review_docs`) with link/schema guards and zero code edits. |
| **[`memory-dream.workflow.yaml`](memory-dream.workflow.yaml)** | `Workflow` · `inherit: none` · `save: none` | Batch repository memory synthesis ("dream") curating `docs/memories/*.md` from archived specs and `run_tombstones`. |
| **[`in-place-refactor.plan.yaml`](in-place-refactor.plan.yaml)** | `Plan` · `git.isolation: in_place` · `save: none` | Unified in-place plan execution (`build` $\to$ `review` $\to$ `deliver`) without `blueprint_synthesis`, red `contract`, or `handoff`. |
| **[`subsystem-feature.milestone.yaml`](subsystem-feature.milestone.yaml)** | `Milestone` · `inherit: code` · `save: commit` | Full subsystem delivery (`blueprint_synthesis` $\to$ `tasks:` with `code.seam` checks $\to$ `verify`/`polish`/`integration_review`/`handoff`). |

> [!NOTE]
> Standalone `*.workflow.yaml` scenario examples use `inherit: none` and omit local CLI `runner:` shell strings so they validate cleanly as portable templates. Configure `roles.<name>.runner` or switch to `inherit: workspace` before launching automated `acdd run`.

Canonical baseline structural examples ([`profile-workflow.example.yaml`](profile-workflow.example.yaml), [`profile-plan.example.yaml`](profile-plan.example.yaml), [`profile-milestone.example.yaml`](profile-milestone.example.yaml)) and authoring workflows ([`author-milestone.workflow.yaml`](author-milestone.workflow.yaml), [`author-plan.workflow.yaml`](author-plan.workflow.yaml), [`author-workflow.workflow.yaml`](author-workflow.workflow.yaml)) remain alongside the domain catalog below.

---

## 2. Domain Catalog Profiles

| Profile / Manifest File | Target Domain & Tech Stack | Key Architectural Features |
| :--- | :--- | :--- |
| **[`fullstack-commerce-django-playwright.yaml`](fullstack-commerce-django-playwright.yaml)** | Python / Django (ORM, HTMX) + Playwright MCP e2e browser tests | Multi-tier validation: fast scoped backend checks, automated migration dry-run, browser seam verification, and N+1 query contour audits. |
| **[`test-suite-refactor.yaml`](test-suite-refactor.yaml)** | Typed test suite refactoring (aligned with `test-suite-refactor` skill) | Collect-only inventory freezing, duplicate subsumption, strict typing/lint verification, and zero-coverage-loss audit. |
| **[`documentation-graph-refactor.yaml`](documentation-graph-refactor.yaml)** | Documentation graph refactor & ADR reconciliation (aligned with `agent-instructions` skill) | Drift scanning against live signatures, strictly bounded docs/ edits with zero code tampering guard, clickable link validation, and cognitive clarity review. |
| **[`legacy-plans-to-milestones-migration.yaml`](legacy-plans-to-milestones-migration.yaml)** | Legacy plans & tasks to spec DAG migration (aligned with `planner` skill) | Legacy markdown plan decomposition, topological DAG cycle detection, formal frontmatter/invariant generation, and zero code tampering enforcement. |
| **[`infrastructure-deploy-database.yaml`](infrastructure-deploy-database.yaml)** | DevOps, Terraform, Kube, DB migrations & live rolling release | Preflight dry-run gates, state snapshot checkpoints, forward migration execution, synthetic smoke checks, and automated rollback routing. |
| **[`multirepo-umbrella-cross-service.yaml`](multirepo-umbrella-cross-service.yaml)** | Multi-repo umbrella coordination (backend, frontend, infra) | Upfront cross-service contract synthesis, parallel worktree task delivery, umbrella integration gate, and synchronized handoff. |
| **[`seam-logger-and-event-telemetry.yaml`](seam-logger-and-event-telemetry.yaml)** | Seam telemetry logger & external observability bus dispatch | Heterogeneous task coordination (`when: code` / `when: docs`), `checks: [code.seam]`, and autonomous 0-token Command Gates emitting webhook events and `acdd blackboard` notes. |
| **[`milestone-manifest-with-skills.md.example`](milestone-manifest-with-skills.md.example)** | Spec manifest demonstrating `required_skills` & `repositories` | Frontmatter declaration of required agent skills and `linked_workspaces` repository bindings. |

---

## 3. Multi-Repository Lifecycle Management Cycle

When an initiative spans multiple repositories (e.g. `commerce-backend`, `commerce-frontend`, and `infrastructure`):

### The Coordination Architecture

```
                      ┌────────────────────────────────────────┐
                      │ ContextUnity ACDD Umbrella Workspace   │
                      │  (Multi-Repo acdd.yaml Index)          │
                      └───────────────────┬────────────────────┘
                                          │
                  ┌───────────────────────┼───────────────────────┐
                  ▼                       ▼                       ▼
      ┌───────────────────────┐ ┌───────────────────┐ ┌────────────────────────┐
      │  commerce-backend     │ │ commerce-frontend │ │   commerce-infra       │
      │  branch: feat/cart-v2 │ │ branch: feat/cart-v2│ │ branch: feat/cart-v2  │
      └───────────┬───────────┘ └─────────┬─────────┘ └───────────┬────────────┘
                  │                       │                       │
                  └───────────────────────┼───────────────────────┘
                                          │
                                          ▼
                      ┌────────────────────────────────────────┐
                      │    Blueprint Synthesis & Cross-Repo    │
                      │         State Engine (`state.sqlite`)  │
                      │  • Upfront Contract Synthesis Gate     │
                      │  • Cross-repo Schema & Seam Checks     │
                      └────────────────────────────────────────┘
```

### Upfront Run Initialization (`acdd start`)

Before dispatching autonomous agent workers, start the run from the umbrella workspace:

```bash
acdd start docs/milestones/AC-001-commerce-checkout.md --worktree
```

**What `acdd start` performs before dispatch**:
1. **Profile & Schema Validation**: Validates `version: 2` profile, `DomainProvider` capabilities (`call`, `checks`, `context`, plugin `tools`), and spec dependencies (`depends_on`, including read-only cross-workspace tombstones).
2. **Multi-Repo Path Confinement**: Validates `repositories:` against `linked_workspaces` in `acdd.yaml` with symlink rejection (`ADR-0002`).
3. **SQLite Workflow Registration**: Admits the run graph (`runs`, `gate_nodes`, `task_dependencies`, `subtasks`) into `<primary_worktree>/.acdd/state.sqlite`.
