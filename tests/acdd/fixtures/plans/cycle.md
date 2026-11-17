---
id: PLAN-CYCLE
title: Cyclic Plan
doc_type: plan
---

# Cyclic Plan

### task: first-step
```yaml
task_ref: first-step
target: Validate cycles
scope: [src/first.rs]
depends_on: [second-step]
status: planned
```

### task: second-step
```yaml
task_ref: second-step
target: Close the cycle
scope: [src/second.rs]
depends_on: [first-step]
status: planned
```
