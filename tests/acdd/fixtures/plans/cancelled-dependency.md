---
id: PLAN-CANCELLED-DEP
title: Cancelled Dependency
doc_type: plan
---

# Cancelled Dependency

### task: first-step
```yaml
task_ref: first-step
target: Cancelled prerequisite
scope: [src/first.rs]
status: cancelled
```

### task: second-step
```yaml
task_ref: second-step
target: Require a completed prerequisite
scope: [src/second.rs]
depends_on: [first-step]
status: planned
```
