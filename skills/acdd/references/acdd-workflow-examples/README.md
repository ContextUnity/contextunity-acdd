# ACDD Workflow Examples

These templates provide ready-to-copy standalone and `inherit: audit` workflows (`skills/acdd/references/acdd-workflow-examples/`). Embedded binary workflows stay strictly limited to `base`, `code`, and `documentation`; copy or reference these templates under `.acdd/workflows/` when running `audit`, `spec_planning`, `hotfix`, `memory_optimize`, or specialized review pipelines.

| Example | Inherit / Mode | Specialization & Output |
| :--- | :--- | :--- |
| [`audit.yaml`](audit.yaml) | `inherit: base` · `in_place`, `save: none` | Read-only 3-phase review and taint pipeline (`recon -> analyze -> triage`) emitting `.acdd/reports/{run_name}.md`. |
| [`spec_planning.yaml`](spec_planning.yaml) | `inherit: base` · `in_place`, `save: none` | Read-only specification/milestone synthesis (`synthesize -> review`) aware of `acdd.yaml` `specs.*` entries. |
| [`hotfix.yaml`](hotfix.yaml) | `inherit: default` | Lightweight localized bugfix workflow (`goal.build -> goal.review -> goal.deliver -> run_tests -> handoff`). |
| [`memory_optimize.yaml`](memory_optimize.yaml) | `inherit: base` · `worktree`, `save: commit` | Consolidates unprocessed declarative retrospectives (`docs/memories/.retro/<workflow>/`) into flat Git-tracked `docs/memories/<domain>.md` + `docs/memories/instructions-and-skills.md` under independent review. |
| [`security-sast.workflow.yaml`](security-sast.workflow.yaml) | `inherit: audit` · `gates_patch:` | OWASP injection, authz, path-traversal, and secret-sink source-to-sink taint audit emitting JSON at `.acdd/reports/{run_name}-sast.json`. |
| [`pr-code-review.workflow.yaml`](pr-code-review.workflow.yaml) | `inherit: audit` · `gates_patch:` | Pre-merge PR review checking correctness, concurrency/locks, test seams, duplicates, and dead tails emitting Markdown at `.acdd/reports/{run_name}-review.md`. |
| [`architecture-hygiene.workflow.yaml`](architecture-hygiene.workflow.yaml) | `inherit: audit` · `gates_patch:` | Whole-repo architecture audit (`INV-SINGLE-OWNER`, dependency cycles, symbol clones, and phantom-tested dead tails) emitting Markdown at `.acdd/reports/{run_name}-hygiene.md`. |

Validate standalone templates directly, and copy `audit.yaml` alongside any `inherit: audit` template into `.acdd/workflows/` before validating or running `inherit: audit` overlays:

```sh
acdd validate skills/acdd/references/acdd-workflow-examples/audit.yaml
acdd validate skills/acdd/references/acdd-workflow-examples/spec_planning.yaml
```

After copying `audit.yaml` and a specialization into `.acdd/workflows/`, validate and run it ephemerally against a target goal or scope without modifying the working tree:

```sh
acdd validate .acdd/workflows/security-sast.workflow.yaml
acdd start --goal "Audit authentication and file path handling" --workflow security-sast.workflow
```
