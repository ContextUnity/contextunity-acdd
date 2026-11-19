# Companion Agent Skills for ContextUnity ACDD

This directory contains the reusable companion skills for AI coding agents working in ACDD-indexed repositories.

---

## Skills Index

| Skill | Description | Primary Phase |
|---|---|---|
| [`acdd`](acdd/SKILL.md) | **Primary ACDD Skill**: Navigation of ACDD MCP/CLI tools, graph discovery/hygiene (`duplicates`, `dead_tails`), profile & task lifecycle, and blackboard coordination. | All Sessions |
| [`contract-planning`](contract-planning/SKILL.md) | Architectural viability audit, ADR/invariant reconciliation, risk probing, and end-to-end contract tracing before task authoring or amendments. | Spec & Blueprint Synthesis |
| [`test-suite-rules`](test-suite-rules/SKILL.md) | Authoring, deduplicating, and refactoring tests through real production seams, table-driven contract matrices, lowest-cost isolation, and leak-free harnesses. | Testing & Refactoring |
| [`close-review-findings`](close-review-findings/SKILL.md) | Classifying review findings, clustering by canonical owner invariant, generalizing defects without test bloat, and verifying closure. | Review & Defect Remediation |
| [`agent-instructions`](agent-instructions/SKILL.md) | Standards for authoring and revising `AGENTS.md`, `SKILL.md`, and present-tense project documentation. | Governance & Docs |
| `review-authority` *(external prerequisite)* | Governs finding admission and authority boundaries. Required by `close-review-findings`. | Review & Repair Authority |

---

## Installation & Setup

Install the external `review-authority` prerequisite from [shared agent skills](https://github.com/alpiua/dot-agents) before using `close-review-findings`:

```sh
git clone --depth 1 https://github.com/alpiua/dot-agents.git /tmp/acdd-shared-agent-skills
mkdir -p ~/.agents/skills
cp -r /tmp/acdd-shared-agent-skills/skills/review-authority ~/.agents/skills/
```

Sync all bundled skills to `~/.agents/skills/` and detected agent directories (Antigravity/Gemini, Codex, Claude, Cursor):

```sh
./skills/install.sh
```

In your repository's `AGENTS.md`, route agents to these skills using [skills/acdd/references/agents.md.example](acdd/references/agents.md.example).
