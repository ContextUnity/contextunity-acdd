---
title: "Workflows"
doc_type: guide
---

# Workflows

The `docs/workflows/` directory contains standalone and workspace-inherited `version: 2` gate profiles (`.workflow.yaml`) discovered via `profiles_dirs` in `acdd.yaml`.

## Overview

> [!NOTE]
> `acdd start --goal "<goal>" --profile <name>` executes an ephemeral standalone workflow directly in the active workspace without creating spec files. See the [CLI reference](../reference/cli.md) for lifecycle commands and persistence modes.

In `0.6.0`, every execution mode—standalone taskless workflows, in-place plans, and worktree-isolated milestones—runs on the unified `GateProfile` state engine (`crates/acdd-state`) configured through `git:` and `gates:`:

- **Taskless Workflows (`inherit: none`, `save: none`)**: Pure root-gate state machines with zero `tasks:` blocks and zero Git commits.
- **In-Place Plans (`inherit: in_place`)**: `git.isolation: in_place`, `save: snapshot`, executing tasks directly in the active checkout without spawning worktrees.
- **Worktree Milestones (`inherit: default` or `inherit: code`)**: Full 3-tier `blueprint_synthesis` $\to$ `tasks:` DAG waves $\to$ post-task closure (`verify`, `polish`, `integration_review`, `handoff`).
- **Use Cases**: Continuous quality checks, polish loops, PR review automation, scheduled health audits, and lifecycle test suites.

## Example & Test Profiles

The authoring and verification profiles below validate against `acdd_state::profile`:

- [author-milestone.workflow.yaml](author-milestone.workflow.yaml): Standalone `synthesize -> review -> preflight` milestone authoring workflow.
- [author-plan.workflow.yaml](author-plan.workflow.yaml): Standalone `synthesize -> review -> preflight` plan authoring workflow.
- [author-workflow.workflow.yaml](author-workflow.workflow.yaml): Standalone `synthesize -> review -> preflight` workflow profile authoring workflow.
- [code-quality-and-polish.workflow.yaml](code-quality-and-polish.workflow.yaml): Standalone `verify -> polish -> review` quality gate.
- [test-workflow-lifecycle.workflow.yaml](test-workflow-lifecycle.workflow.yaml): Test profile specifying the pure taskless Workflow model and repair routing.
- [test-plan-lifecycle.workflow.yaml](test-plan-lifecycle.workflow.yaml): Test profile specifying the Plan in-place execution model (`git.isolation: in_place`, `save: snapshot`, strictly `tasks:` container).
- [test-milestone-lifecycle.workflow.yaml](test-milestone-lifecycle.workflow.yaml): Test profile specifying the full 3-tier Milestone lifecycle (`blueprint_synthesis` $\to$ task DAG $\to$ `verify` / `polish` / `integration_review` $\to$ `handoff`).
