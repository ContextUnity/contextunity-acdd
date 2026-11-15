# Hierarchical ACDD Profile Catalog & Domain Examples

This catalog provides domain-tailored ACDD profiles, lifecycle presets, and scenario examples implementing [ADR 0020](../../../../docs/adr/0020-hierarchical-acdd-gates-and-milestone-orchestration.md) and [ADR 0021](../../../../docs/adr/0021-workflow-plan-and-milestone-lifecycle-hierarchy.md).

---

## 1. Scenario Examples (`{scenario}.{workflow|plan|milestone}.yaml`)

| Scenario File | Lifecycle & Inheritance | Purpose |
| :--- | :--- | :--- |
| **[`pentest-security-audit.workflow.yaml`](pentest-security-audit.workflow.yaml)** | `Workflow` · `inherit: none` · `save: log` | Self-contained penetration & security audit pipeline (recon, exploit probe, remediation, independent signoff) with zero `.acdd/profile.yaml` dependency. |
| **[`ci-verification-gate.workflow.yaml`](ci-verification-gate.workflow.yaml)** | `Workflow` · `inherit: none` · `save: log` | Clean-room standalone CI verification & autonomous repair loop for release/PR gates. |
| **[`repo-polish.workflow.yaml`](repo-polish.workflow.yaml)** | `Workflow` · resource overlay (`AC-004`: `inherit: workspace`) · `save: log` | Shared-resource workflow reusing workspace roles, models, and commands while declaring its own stateless `gates:` graph. |
| **[`docs-refactor-and-reconcile.workflow.yaml`](docs-refactor-and-reconcile.workflow.yaml)** | `Workflow` · `inherit: none` · `save: log` | Refactors and synchronizes docs, `AGENTS.md`, skills, and examples (`audit_drift` $\to$ `reconcile_docs` $\to$ `review_docs`) with link/schema guards and zero code edits. |
| **[`in-place-refactor.plan.yaml`](in-place-refactor.plan.yaml)** | `Plan` · `save: snapshot` | Unified `TasksStore` in-place plan execution (`build` $\to$ `review` $\to$ `deliver`) without `blueprint_synthesis`, red `contract`, or `handoff`. |
| **[`subsystem-feature.milestone.yaml`](subsystem-feature.milestone.yaml)** | `Milestone` · `inherit: default` · `save: commit` | Full subsystem delivery (`blueprint_synthesis` $\to$ `tasks:` with `contract`/`seam` $\to$ `verify`/`polish`/`milestone_review`/`handoff`). |

> [!NOTE]
> The four standalone `*.workflow.yaml` scenario examples above use `inherit: none` and omit local CLI `runner:` shell strings so they validate cleanly as portable templates. Configure `roles.<name>.runner` (or switch to `inherit: workspace` under `AC-004`) before executing them with `acdd workflow run`.

Canonical baseline structural examples ([`profile-workflow.example.yaml`](profile-workflow.example.yaml), [`profile-plan.example.yaml`](profile-plan.example.yaml), [`profile-milestone.example.yaml`](profile-milestone.example.yaml)) and authoring workflows ([`author-milestone.workflow.yaml`](author-milestone.workflow.yaml), [`author-plan.workflow.yaml`](author-plan.workflow.yaml), [`author-workflow.workflow.yaml`](author-workflow.workflow.yaml)) remain alongside the domain catalog below.

---

## 2. Domain Catalog Profiles

| Profile / Manifest File | Target Domain & Tech Stack | Key Architectural Features |
| :--- | :--- | :--- |
| **[`fullstack-commerce-django-playwright.yaml`](fullstack-commerce-django-playwright.yaml)** | Python / Django (ORM, HTMX) + Playwright MCP e2e browser tests | Multi-tier validation: fast scoped backend checks, automated migration dry-run, browser seam verification, and N+1 query contour audits. |
| **[`test-suite-refactor.yaml`](test-suite-refactor.yaml)** | Typed test suite refactoring (aligned with `test-suite-refactor` skill) | Collect-only inventory freezing, duplicate subsumption, strict typing/lint verification, and zero-coverage-loss audit. |
| **[`documentation-graph-refactor.yaml`](documentation-graph-refactor.yaml)** | Documentation graph refactor & ADR reconciliation (aligned with `agent-instructions` skill) | Drift scanning against live signatures, strictly bounded docs/ edits with zero code tampering guard, clickable link validation, and cognitive clarity review. |
| **[`legacy-plans-to-milestones-migration.yaml`](legacy-plans-to-milestones-migration.yaml)** | Legacy plans & tasks to milestone DAG migration (aligned with `planner` skill) | Legacy markdown plan decomposition, topological DAG cycle detection, formal frontmatter/invariant generation, and zero code tampering enforcement. |
| **[`infrastructure-deploy-database.yaml`](infrastructure-deploy-database.yaml)** | DevOps, Terraform, K8s, DB migrations & live rolling release | Preflight dry-run gates, state snapshot checkpoints, forward migration execution, synthetic smoke checks, and automated rollback routing. |
| **[`multirepo-umbrella-cross-service.yaml`](multirepo-umbrella-cross-service.yaml)** | Multi-repo umbrella coordination (backend, frontend, infra) | Upfront cross-service contract synthesis, parallel worktree task delivery, umbrella integration gate, and atomic synchronized handoff. |
| **[`seam-logger-and-event-telemetry.yaml`](seam-logger-and-event-telemetry.yaml)** | Seam telemetry logger & external observability bus dispatch | Heterogeneous task coordination (Code + Doc + Observer), pure topological `when: seam`, and autonomous 0-token Command Gates emitting webhook events and blackboard signals. |
| **[`milestone-manifest-with-skills.md.example`](milestone-manifest-with-skills.md.example)** | Manifest specification demonstrating `required_skills` | Frontmatter declaration of required agent skills, fail-closed `TASK_SKILL_MISSING` validation, and targeted context injection. |

---

## 3. Multi-Repository Lifecycle Management Cycle

When an initiative spans multiple repositories (e.g. `commerce-backend`, `commerce-frontend`, and `infrastructure`):

### The Coordination Architecture

```
                      ┌────────────────────────────────────────┐
                      │ ContextUnity ACDD Umbrella Workspace   │
                      │  (Multi-Repo acdd.yaml Index)     │
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
                      │    Milestone Contract Cross-Matrix     │
                      │       (milestone_contract_matrix)      │
                      │  • Upfront Contract Synthesis Gate     │
                      │  • Cross-repo Seam Mock / Schema Sync  │
                      └────────────────────────────────────────┘
```

### Upfront Milestone Initialization (`milestone init`)

Before dispatching autonomous agent workers, the coordinator/orchestrator executes preflight initialization:

```bash
acdd milestone init docs/milestones/060-hierarchical-acdd-gates-and-milestone-orchestration.md
```

**What `milestone init` performs before dispatch**:
1. **Host Skill Preflight Check**: Validates skills declared in `required_skills:` against repository and host installations. Preflight errors use `MILESTONE_PREFLIGHT_FAILED`.
2. **Multi-Repo Path & Branch Validation**: Reads `repositories:` from manifest frontmatter, confirms all participant repositories exist, and checks that target base branches are clean.
3. **Automated Base Worktree Scaffolding**: If not running in `--in-place` mode, atomically creates milestone base branches (`milestone/<id>`) and base worktrees (`.worktrees/<id>-<slug>`) across all declared repositories in a single operation.
4. **SQLite Workflow Registration**: Admits tasks, registers participant repositories, and populates initial `gate_transitions` root state in `.acdd/tasks.sqlite`.

---

### The 4-Stage Multi-Repo Control Cycle:

1. **Preflight Manifest Initialization (`milestone init`)**:
   - Reads `repositories:` and `required_skills:` upfront.
   - Sets up worktrees and base branches automatically across backend, frontend, and infra.

2. **Upfront Cross-Repo Synthesis (`blueprint_synthesis`)**:
   - The architect runs `blueprint_synthesis.synthesize` (and `blueprint_synthesis.review` approves it) at the umbrella workspace level.
   - Declares boundary interfaces (e.g. backend publishes OpenAPI schema / HTMX endpoint contract to SQLite `milestone_contract_matrix`).
   - Consumer repositories (frontend) bind their seam tests to this published contract schema before backend implementation starts.

3. **Synchronized Parallel Execution in Scaffolded Worktrees**:
   - Agents work in pre-scaffolded parallel worktrees: `.worktrees/<milestone-prefix>-<repo-name>-<task-slug>`.
   - Each repository's task advances through its own local scoped command gates (`check_task`).
   - SQLite tracks transitions independently under compound breadcrumbs: `{milestone_id}/{repo_id}/{task_id}/{gate_path}`.

4. **Autonomous Cross-Repo Integration Gate (`cross_repo_verify`) & Atomic Merkle Handoff**:
   - Umbrella Command Gate runs integration tests across live services (e.g. Playwright browser tests hitting the live Django backend).
   - Milestone handoff calculates a joint cryptographic receipt binding the Git tree SHAs of all participant repositories before atomic fast-forward landing.

---

## 4. Proposal: Manifest `required_skills` Specification

To eliminate agent hallucination and avoid token-wasting discovery loops across 30+ installed skills, milestone and plan manifests can explicitly declare the skills required for execution:

### Manifest Syntax

```yaml
---
id: m-commerce-checkout-redesign
title: "Commerce Checkout Redesign"
doc_type: contract
status: active
owners:
  - backend/
  - frontend/
# NEW: Explicit Skill Governance
required_skills:
  - acdd       # Base code map, ast grep, and task operations
  - django-gridviewspec      # Django ORM, HTMX, and GridViewSpec standards
  - playwright               # Browser testing and DOM interaction
  - test-suite-refactor      # Test suite consolidation and coverage preservation
---
```

### Engine Integration Invariants:

1. **Fail-Closed Claim Check (`TASK_SKILL_MISSING`)**:
   When an agent executes `task claim` or `task_manage(action="claim")`:
   ACDD checks `~/.agents/skills/` and `.agents/skills/`. If any skill named in `required_skills` is missing or not indexed, claim fails immediately with:
   `TASK_SKILL_MISSING: Skill 'django-gridviewspec' is required by manifest 'm-commerce-checkout-redesign' but not found in active skill registries. Do: install or enable the skill before claiming.`

2. **No Markdown Dumping (Lean Metadata Only)**:
   ACDD does **NOT** inject or dump raw skill markdown bodies into the task claim bundle. Invariants, instructions, and `AGENTS.md` already govern agent execution. `workflow_guidance.active_skills` simply returns the lean array of admitted skill names `["acdd", "django-gridviewspec"]`, acting as an authoritative whitelist without burning context tokens.

3. **Zero Hallucination & Lean Overhead**:
   Agents know exactly which specialized toolsets and standards apply to the task, without guessing or scanning through dozens of unrelated global skills.
