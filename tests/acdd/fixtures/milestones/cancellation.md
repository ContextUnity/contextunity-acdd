---
id: m-cancellation
title: Mixed task outcomes
doc_type: contract
status: active
---
# Mixed task outcomes

### task: planned
```yaml
task_ref: planned
target: Deliver planned work
scope: [src/planned.rs]
status: planned
```

### task: delivered
```yaml
task_ref: delivered
target: Preserve a completed task
scope: [src/delivered.rs]
status: completed
receipt:
  run_id: run-cancel-1
  node_cursor: "task:delivered:deliver"
  gate_id: deliver
  worker_id: builder-1
  claim_revision: 1
  contract_revision: 1
  candidate_tree: "0123456789abcdef0123456789abcdef01234567"
  accepted_at: "2026-10-01T10:00:00.000Z"
  rollup:
    review_summary: "public-seam verification"
```

### task: cancelled
```yaml
task_ref: cancelled
target: Preserve cancelled work
scope: [src/cancelled.rs]
status: cancelled
```
