---
id: PLAN-LINEAR
title: "Canonical Linear Plan"
doc_type: plan
spec: "Test fixture for linear two-step in-place plan execution"
depends_on: []
---

# Canonical Linear Plan

### task: step-1
```yaml
task_ref: step-1
target: Implement first action
scope: [src/first.rs]
status: planned
```

### task: step-2
```yaml
task_ref: step-2
target: Implement second action
scope: [src/second.rs]
depends_on: [step-1]
status: planned
```
