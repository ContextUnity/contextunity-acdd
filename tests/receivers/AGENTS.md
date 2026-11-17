# Value Flow Receivers Test Suite

Subtree tests for receiver inference, type propagation, and method dispatch.

## Routing and Architecture

Tests are divided by language backend into focused subdirectories:

- `rust/`: Rust field chains, trait dispatch, pointer receivers, builtin receivers, value-flow cases, and cross-file module paths.
- `python/`: Python bindings, branch joins, factories, collections, member resolution, and inheritance cases.

## Non-Negotiable Invariants

- Public value-flow integration cases live here; private reducer unit tests stay beside source code.
- Every receiver claim must test exact resolution separately from possible targets.
- Exercise complete joins, incomplete joins, and unknown alternatives; possible targets must never appear as exact `calls` edges.
- Split test files strictly by logical domain and contract boundaries;
  avoid artificial splits solely for line count.

Tests in `tests/receivers/{rust,python}` are wired through `tests/linker/receivers/`
inside the existing `linker` Cargo target; focused commands such as
`rust::field_chains::` and `python::branch_joins::` filter backend modules directly.

## Verification Commands

Run language-specific receiver suites:
```bash
cargo test --test linker rust::
cargo test --test linker python::
```
