---
id: PLAN-DAG
title: "Canonical DAG Plan"
doc_type: plan
spec: "Test fixture for branching DAG in-place plan execution"
depends_on: []
---

# Canonical DAG Plan

### task: step-a
```yaml
task_ref: step-a
target: Core changes
scope: [src/core.rs]
status: planned
```

### task: step-b
```yaml
task_ref: step-b
target: Extension on core
scope: [src/beta.rs]
depends_on: [step-a]
status: planned
```

### task: step-c
```yaml
task_ref: step-c
target: Alternative extension on core
scope: [src/gamma.rs]
depends_on: [step-a]
status: planned
```
