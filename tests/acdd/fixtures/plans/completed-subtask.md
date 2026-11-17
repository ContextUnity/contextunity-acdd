---
id: PLAN-COMPLETED-SUBTASK
title: Completed Subtask
doc_type: plan
---

# Completed Subtask

### task: guarded-step
```yaml
task_ref: guarded-step
target: Preserve evidenced completion
scope: [src/first.rs]
status: planned
subtasks:
  - subtask_ref: check
    title: Verify action
    status: completed
    evidence: Canonical fixture verification
```
