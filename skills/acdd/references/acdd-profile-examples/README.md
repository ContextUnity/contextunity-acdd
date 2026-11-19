# ACDD Audit Profile Examples (`inherit: audit`)

These three examples specialize the built-in `audit` profile (`recon -> analyze -> triage`, `git.isolation: in_place`, `save: none`) using `gates_patch:` and explicit `save.report` targets.

| Example | Inherit / Mode | Specialization & Output |
| :--- | :--- | :--- |
| [`security-sast.workflow.yaml`](security-sast.workflow.yaml) | `inherit: audit` · `gates_patch:` | OWASP injection, authz, path-traversal, and secret-sink source-to-sink taint audit emitting JSON at `.acdd/reports/{run_name}-sast.json`. |
| [`pr-code-review.workflow.yaml`](pr-code-review.workflow.yaml) | `inherit: audit` · `gates_patch:` | Pre-merge PR review checking correctness, concurrency/locks, test seams, duplicates, and dead tails emitting Markdown at `.acdd/reports/{run_name}-review.md`. |
| [`architecture-hygiene.workflow.yaml`](architecture-hygiene.workflow.yaml) | `inherit: audit` · `gates_patch:` | Whole-repo architecture audit (`INV-SINGLE-OWNER`, dependency cycles, symbol clones, and phantom-tested dead tails) emitting Markdown at `.acdd/reports/{run_name}-hygiene.md`. |

All three templates bind zero shell commands and validate standalone:

```sh
acdd validate skills/acdd/references/acdd-profile-examples/security-sast.workflow.yaml
acdd validate skills/acdd/references/acdd-profile-examples/pr-code-review.workflow.yaml
acdd validate skills/acdd/references/acdd-profile-examples/architecture-hygiene.workflow.yaml
```

Run any workflow ephemerally against a target goal or scope without modifying the working tree:

```sh
acdd start --goal "Audit authentication and file path handling" --profile skills/acdd/references/acdd-profile-examples/security-sast.workflow.yaml
```
