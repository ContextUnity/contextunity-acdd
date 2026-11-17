---
id: m-linear
title: Sequential milestone
doc_type: contract
status: active
---
# Sequential milestone

### task: first
```yaml
task_ref: first
target: Deliver the first task
scope: [src/first.rs]
```

### task: second
```yaml
task_ref: second
target: Deliver the second task
scope: [src/second.rs]
depends_on: [first]
```
