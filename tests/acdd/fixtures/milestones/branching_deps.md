---
id: m-branching
title: Branching milestone
doc_type: contract
status: active
---
# Branching milestone

### task: first
```yaml
task_ref: first
target: Prepare the shared foundation
scope: [src/first.rs]
```

### task: second
```yaml
task_ref: second
target: Deliver the first branch
scope: [src/second.rs]
depends_on: [first]
```

### task: third
```yaml
task_ref: third
target: Deliver the second branch
scope: [src/third.rs]
depends_on: [first]
```

### task: final
```yaml
task_ref: final
target: Integrate both branches
scope: [src/final.rs]
depends_on: [second, third]
```
