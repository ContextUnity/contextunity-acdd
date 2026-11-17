---
id: PLAN-PENDING-SUBTASK
title: Pending Subtask
doc_type: plan
---

# Pending Subtask

### task: guarded-step
```yaml
task_ref: guarded-step
target: Require terminal subtasks
scope: [src/first.rs]
status: planned
subtasks:
  - subtask_ref: check
    title: Verify action
    status: pending
```
