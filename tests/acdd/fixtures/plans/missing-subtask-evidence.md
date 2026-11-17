---
id: PLAN-MISSING-SUBTASK-EVIDENCE
title: Missing Subtask Evidence
doc_type: plan
---

# Missing Subtask Evidence

### task: guarded-step
```yaml
task_ref: guarded-step
target: Require completion evidence
scope: [src/first.rs]
status: planned
subtasks:
  - subtask_ref: check
    title: Verify action
    status: completed
```
