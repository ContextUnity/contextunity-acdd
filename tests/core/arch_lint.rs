use crate::common::Workspace;
use acdd_core::config::load_adapter;
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::response::{QueryOptions, ResponsePolicy};
use acdd_index::db::{
    cycles,
    lint::{enforced_invariant_ids, run_lint, syntax_paged, LintRequest},
    reader::{self, analyze_paged},
    traversal::{dead_tails_paged, duplicates_paged},
};
use acdd_index::docs;
use acdd_index::models::details::NodeDetailKey;
use contextunity_acdd::engine::pipeline;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn page(limit: usize, offset: usize, generation: Option<&str>) -> QueryOptions {
    QueryOptions::resolve(
        &ResponsePolicy::default(),
        Some(limit),
        offset,
        None,
        generation.map(str::to_owned),
    )
    .unwrap()
}

#[test]
fn docs_acdd_rule_extraction_and_cold_cache_delta_merkle_parity() {
    let zones = BTreeSet::from(["core".to_owned(), "app".to_owned()]);
    let doc_src = r#"---
title: Architecture Rules
doc_type: architecture
---
# Layering

> [!IMPORTANT]
> Invariant: `core` must not depend on `app`.
> INV-CORE-LEAF: [`leaf`](../src/core/leaf.rs)

```acdd-rule
id: INV-CORE-LEAF
invariant: INV-CORE-LEAF
severity: error
kind: forbid_dependency
from: { zones: ["core"] }
to: { zones: ["app"], external_symbols: ["rusqlite"] }
```

```markdown
- INV-FENCED-EXAMPLE: fenced examples must never become declared invariants.
```

```acdd-rule
id: BAD-YAML
kind: [unclosed
```

```acdd-rule
id: BAD-ZONE
kind: forbid_dependency
from: { zones: ["missing_zone"] }
to: { zones: ["core"] }
```

```acdd-rule
id: INV-CORE-LEAF
kind: doc_integrity
```
"#;
    let extracted =
        docs::extract_with_details_with_zones("docs/arch.md", doc_src, 0.0, &[], Some(&zones))
            .unwrap();
    assert_eq!(extracted.len(), 1);
    let (doc, details) = &extracted[0];
    assert_eq!(
        details["declared_invariants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["INV-CORE-LEAF"],
        "fenced CodeBlock INV-* example must be skipped: {details}"
    );
    assert!(doc.is_invariant);
    let rules = details[NodeDetailKey::ARCH_RULES].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["rule"]["id"], "INV-CORE-LEAF");
    assert!(rules[0]["line"].as_u64().unwrap() > 1);
    let rule_errors = details["arch_rule_errors"].as_array().unwrap();
    assert_eq!(rule_errors.len(), 3, "{rule_errors:?}");

    // Untouched docs omit arch_rules completely.
    let plain = docs::extract_with_details("docs/plain.md", "# Plain\nText.\n", 0.0, &[]).unwrap();
    assert!(plain[0].1.get(NodeDetailKey::ARCH_RULES).is_none());

    // Cold, warm-cached, and delta builds preserve identical output_root across Nodes and Errors.
    let ws = Workspace::new();
    ws.write(
        "acdd.yaml",
        "roots: [.]\narchitecture:\n  zones:\n    core: [\"src/core/**\"]\n    app: [\"src/app/**\"]\n",
    );
    ws.write("src/core/leaf.rs", "pub fn leaf() {}\n");
    ws.write("src/app/main.rs", "pub fn main() {}\n");
    ws.write("docs/arch.md", doc_src);

    let cache_dir = ws.path(".acdd/cache");
    let cold_db = ws.path(".acdd/cold.sqlite");
    let cached_db = ws.path(".acdd/cached.sqlite");
    let delta_db = ws.path(".acdd/delta.sqlite");
    let cold_after_db = ws.path(".acdd/cold_after.sqlite");

    let cold_report =
        pipeline::build_with_cache_dir(ws.root(), &cold_db, None, &cache_dir).unwrap();
    let cached_report =
        pipeline::build_with_cache_dir(ws.root(), &cached_db, None, &cache_dir).unwrap();
    assert_eq!(
        cold_report["output_root"], cached_report["output_root"],
        "cold vs warm-cache Merkle parity"
    );

    std::fs::copy(&cold_db, &delta_db).unwrap();
    ws.write("docs/arch.md", doc_src);
    let delta_report = pipeline::delta_with_cache_dir(
        ws.root(),
        &delta_db,
        &[PathBuf::from("docs/arch.md")],
        &cache_dir,
    )
    .unwrap();
    let cold_after_report =
        pipeline::build_with_cache_dir(ws.root(), &cold_after_db, None, &cache_dir).unwrap();
    assert_eq!(
        cold_after_report["output_root"], delta_report["output_root"],
        "cold vs delta Merkle parity"
    );

    let conn = reader::open(&delta_db, ws.root()).unwrap();
    acdd_index::db::commitments::verify(&conn).unwrap();
    let inv_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM nodes WHERE kind='invariant' AND name='INV-FENCED-EXAMPLE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inv_count, 0);
    let stored_errors: Vec<String> = conn
        .prepare("SELECT message FROM errors WHERE path='docs/arch.md' ORDER BY line, message")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(stored_errors.len(), 3, "{stored_errors:?}");
}

#[test]
fn unified_lint_all_eight_arch_primitives_and_multi_language_matrix() {
    let ws = Workspace::new();
    ws.write(
        "acdd.yaml",
        r#"roots: [.]
architecture:
  zones:
    core: ["src/core/**"]
    app: ["src/app/**"]
    providers: ["src/providers/**"]
    py_domain: ["py/domain/**"]
    py_infra: ["py/infra/**"]
    ts_domain: ["ts/domain/**"]
    ts_infra: ["ts/infra/**"]
  rules:
    - id: INV-ALLOW-APP
      invariant: INV-ALLOW-APP
      severity: error
      kind: allow_only_dependencies
      from: { zones: ["app"] }
      to: { zones: ["core"], external_symbols: ["serde"] }
    - id: INV-NO-APP-CORE-CYCLE
      invariant: INV-NO-APP-CORE-CYCLE
      severity: warn
      kind: forbid_cycles
      zones: ["core", "app"]
"#,
    );

    // Rust files
    ws.write(
        "src/core/mod.rs",
        "pub mod internal;\npub mod leaf;\npub fn core_api() { leaf::leaf_ok(); }\n",
    );
    ws.write(
        "src/core/internal.rs",
        "pub fn hidden_internal() -> i32 { 1 }\n",
    );
    ws.write(
        "src/core/leaf.rs",
        "use rusqlite::Connection;\npub fn leaf_ok() {}\npub fn leaf_bad() { crate::app::entry::app_fn(); let _ = Connection::open_in_memory(); }\n",
    );
    ws.write(
        "src/core/clean_with_test.rs",
        "pub fn clean_prod() -> i32 { 7 }\n#[cfg(test)]\nmod tests {\n    use rusqlite::Connection;\n    #[test]\n    fn test_only_db() { let _ = Connection::open_in_memory(); }\n}\n",
    );
    ws.write(
        "src/core/git_owner.rs",
        "use std::process::Command;\npub fn run_git() { let _ = Command::new(\"git\"); }\n",
    );
    ws.write(
        "src/app/entry.rs",
        "use std::process::Command;\npub fn app_fn() { crate::core::internal::hidden_internal(); crate::core::mod_fn(); crate::providers::impls::prov_helper(); let _ = Command::new(\"git\"); }\n",
    );
    ws.write(
        "src/providers/impls.rs",
        "pub trait DomainTrait { fn id(&self) -> i32; }\npub struct GoodProvider;\nimpl DomainTrait for GoodProvider { fn id(&self) -> i32 { 1 } }\npub struct BadProvider;\npub fn prov_helper() {}\n",
    );

    // Python files
    ws.write(
        "py/domain/service.py",
        "import pandas.DataFrame\nfrom py.infra.repo import load_row\ndef run_service():\n    return load_row()\n",
    );
    ws.write("py/infra/repo.py", "def load_row():\n    return 1\n");
    ws.write(
        "py/providers/all.py",
        "class BaseProvider:\n    pass\nclass GoodPy(BaseProvider):\n    pass\nclass BadPy:\n    pass\n",
    );

    // TypeScript files + package.json so external imports classify as external
    ws.write(
        "package.json",
        "{\"name\":\"fixture\",\"dependencies\":{\"axios\":\"^1.0.0\"}}\n",
    );
    ws.write(
        "ts/domain/handler.ts",
        "import axios from 'axios/core';\nimport { fetchData } from '../infra/client';\nexport function handle() { return fetchData(); }\n",
    );
    ws.write("ts/infra/client.ts", "export function fetchData() { return 1; }\n");
    ws.write(
        "ts/providers/all.ts",
        "export interface ProviderContract { run(): void; }\nexport class GoodTs implements ProviderContract { run() {} }\nexport class BadTs {}\n",
    );

    // Documentation files with executable acdd-rule blocks + doc integrity cases
    ws.write(
        "docs/arch.md",
        r#"---
title: Architecture
doc_type: architecture
---
# Rules

- INV-CORE-LEAF: [`leaf_ok`](../src/core/leaf.rs#L2)
- INV-SINGLE-GIT: [`run_git`](../src/core/git_owner.rs#L2)
- INV-BOUNDARY-CORE: [`core_api`](../src/core/mod.rs#L3)
- INV-MUST-IMPL-RS: [`DomainTrait`](../src/providers/impls.rs#L1)
- INV-MUST-IMPL-PY: [`BaseProvider`](../py/providers/all.py#L1)
- INV-MUST-IMPL-TS: [`ProviderContract`](../ts/providers/all.ts#L1)
- INV-PY-EXT: `py_domain` forbids `pandas` and `py_infra`.
- INV-TS-EXT: `ts_domain` forbids `axios`.
- INV-AST-GIT: forbid `Command::new("git")` outside `git_owner.rs`.
- INV-ORPHAN-UNLINKED: declared without governs link or acdd-rule.

```acdd-rule
- id: INV-CORE-LEAF
  invariant: INV-CORE-LEAF
  severity: error
  kind: forbid_dependency
  from: { zones: ["core"] }
  to: { zones: ["app"], external_symbols: ["rusqlite"] }
- id: INV-SINGLE-GIT
  invariant: INV-SINGLE-GIT
  severity: error
  kind: single_owner
  owner_paths: ["src/core/git_owner.rs"]
  to: { external_symbols: ["std::process::Command"] }
- id: INV-BOUNDARY-CORE
  invariant: INV-BOUNDARY-CORE
  severity: error
  kind: boundary_entry
  to: { zones: ["core"] }
  entrypoints: ["src/core/mod.rs"]
- id: INV-MUST-IMPL-RS
  invariant: INV-MUST-IMPL-RS
  severity: error
  kind: must_implement
  from: { paths: ["src/providers/**"] }
  to: { symbols: ["DomainTrait"] }
- id: INV-MUST-IMPL-PY
  invariant: INV-MUST-IMPL-PY
  severity: error
  kind: must_implement
  from: { paths: ["py/providers/**"] }
  to: { symbols: ["BaseProvider"] }
- id: INV-MUST-IMPL-TS
  invariant: INV-MUST-IMPL-TS
  severity: error
  kind: must_implement
  from: { paths: ["ts/providers/**"] }
  to: { symbols: ["ProviderContract"] }
- id: INV-PY-EXT
  invariant: INV-PY-EXT
  severity: error
  kind: forbid_dependency
  from: { zones: ["py_domain"] }
  to: { zones: ["py_infra"], external_symbols: ["pandas"] }
- id: INV-TS-EXT
  invariant: INV-TS-EXT
  severity: error
  kind: forbid_dependency
  from: { zones: ["ts_domain"] }
  to: { external_symbols: ["axios"] }
- id: INV-AST-GIT
  invariant: INV-AST-GIT
  severity: error
  kind: ast_pattern
  from: { paths: ["src/**"], exclude_paths: ["src/core/git_owner.rs"], languages: ["rust"] }
  ast_pattern: 'Command::new("git")'
- id: INV-DOC-CHECK
  severity: error
  kind: doc_integrity
  from: { paths: ["docs/broken_claim.md"] }
  message: "doc gate"
```
"#,
    );
    ws.write(
        "docs/broken_claim.md",
        "# Broken\n- INV-DOC-ORPHAN: strict error via doc_integrity.\nSee [`NoSuchSymbol`](../src/core/leaf.rs).\n",
    );
    ws.write("docs/dup_a.md", "# Dup A\n- INV-DUP: first.\n");
    ws.write("docs/dup_b.md", "# Dup B\n- INV-DUP: second.\n");
    ws.write(
        "docs/milestones/M-1.md",
        "---\ndoc_type: contract\n---\n# Milestone\n- INV-CORE-LEAF: repeated in contract spec.\n- INV-DUP: repeated in contract spec.\n",
    );
    ws.write(
        "docs/archive/old.md",
        "# Archived\n- INV-DUP: archived duplicate ignored.\nSee [`MissingArchived`](../src/core/leaf.rs).\n",
    );

    ws.build();
    let conn = ws.open();
    let adapter = load_adapter(ws.root(), None).unwrap();

    // 1. Fail-closed when ast_pattern is evaluated with workspace_root: None
    let no_root_err = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: Some(&["INV-AST-GIT".to_owned()]),
            minimum_severity: None,
            architecture: Some(&adapter.architecture),
            workspace_root: None,
            page: &page(50, 0, None),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(no_root_err.contains("workspace_root"), "{no_root_err}");

    // 2. Fallback to persisted MetadataKey::Adapter when architecture: None
    let full_default = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: None, // defaults to LintCategory::DEFAULT_VERIFY = [Arch, Syntax, Docs]
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &page(100, 0, None),
        },
    )
    .unwrap();
    assert_eq!(
        full_default["categories"],
        json!(["arch", "syntax", "docs"])
    );
    let items = full_default["items"].as_array().unwrap();
    let has_rule_at = |rule_id: &str, path_substr: &str| {
        items.iter().any(|item| {
            item["rule_id"] == rule_id
                && item["path"]
                    .as_str()
                    .is_some_and(|p| p.contains(path_substr))
        })
    };

    // Assert every primitive and language fired where expected (and stayed silent on clean cases)
    assert!(has_rule_at("INV-CORE-LEAF", "src/core/leaf.rs"), "{items:?}");
    assert!(
        !has_rule_at("INV-CORE-LEAF", "src/core/clean_with_test.rs"),
        "#[cfg(test)] span must be excluded by default: {items:?}"
    );
    assert!(has_rule_at("INV-ALLOW-APP", "src/app/entry.rs"), "{items:?}");
    assert!(has_rule_at("INV-SINGLE-GIT", "src/app/entry.rs"), "{items:?}");
    assert!(!has_rule_at("INV-SINGLE-GIT", "src/core/git_owner.rs"));
    assert!(has_rule_at("INV-BOUNDARY-CORE", "src/app/entry.rs"), "{items:?}");
    assert!(has_rule_at("INV-MUST-IMPL-RS", "src/providers/impls.rs"), "{items:?}");
    assert!(has_rule_at("INV-MUST-IMPL-PY", "py/providers/all.py"), "{items:?}");
    assert!(has_rule_at("INV-MUST-IMPL-TS", "ts/providers/all.ts"), "{items:?}");
    assert!(has_rule_at("INV-PY-EXT", "py/domain/service.py"), "{items:?}");
    assert!(has_rule_at("INV-TS-EXT", "ts/domain/handler.ts"), "{items:?}");
    assert!(has_rule_at("INV-AST-GIT", "src/app/entry.rs"), "{items:?}");
    assert!(!has_rule_at("INV-AST-GIT", "src/core/git_owner.rs"));
    assert!(has_rule_at("INV-DOC-CHECK", "docs/broken_claim.md"), "{items:?}");
    assert!(
        items.iter().any(|it| {
            it["rule_id"] == "INV-DOC-CHECK"
                && it["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("doc gate: ") && m.contains("NoSuchSymbol"))
        }),
        "doc_integrity custom message must retain diagnostic details: {items:?}"
    );
    assert!(has_rule_at("docs.duplicate_invariant", "docs/dup_a.md"), "{items:?}");
    assert!(has_rule_at("docs.duplicate_invariant", "docs/dup_b.md"), "{items:?}");
    assert!(!has_rule_at("docs.duplicate_invariant", "docs/milestones/M-1.md"));
    assert!(!has_rule_at("docs.duplicate_invariant", "docs/archive/old.md"));
    assert!(has_rule_at("docs.orphan_invariant", "docs/arch.md"), "{items:?}");
    // INV-CORE-LEAF is backed by an acdd-rule -> NOT flagged as orphan.
    assert!(!items.iter().any(|it| {
        it["rule_id"] == "docs.orphan_invariant" && it["invariant"] == "INV-CORE-LEAF"
    }));
    // Every arch violation carries invariant + rule_source { path, line }.
    for item in items.iter().filter(|it| it["category"] == "arch") {
        assert!(item["rule_source"]["path"].as_str().is_some(), "{item}");
        assert!(item["rule_source"]["line"].as_u64().is_some(), "{item}");
    }

    // 3. enforced_invariant_ids returns both config and doc rule invariants
    let enforced = enforced_invariant_ids(&conn, &adapter.architecture.rules).unwrap();
    for expected in ["INV-ALLOW-APP", "INV-CORE-LEAF", "INV-AST-GIT", "INV-PY-EXT", "INV-TS-EXT"] {
        assert!(enforced.contains(expected), "missing {expected} in {enforced:?}");
    }

    // 4. Scope filtering, category non-leak, --rule doc_integrity filtering, [linked] validation & pagination
    let scoped = run_lint(
        &conn,
        &LintRequest {
            target: Some("py/"),
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: None,
            minimum_severity: Some(LintSeverity::Error),
            architecture: Some(&adapter.architecture),
            workspace_root: Some(ws.root()),
            page: &page(2, 0, None),
        },
    )
    .unwrap();
    assert!(scoped["items"].as_array().unwrap().iter().all(|it| {
        it["path"].as_str().unwrap().starts_with("py/") && it["category"] == "arch"
    }));
    assert!(
        scoped["summary"]["by_category"].get("docs").is_none(),
        "categories=[Arch] must not leak docs summary key: {scoped}"
    );
    assert_eq!(scoped["summary"]["warnings"], 0);

    // Filtered doc_integrity must still know all loaded acdd-rule invariants (no false orphan_invariant).
    let doc_rule_only = run_lint(
        &conn,
        &LintRequest {
            target: Some("docs/"),
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: Some(&["INV-DOC-CHECK".to_owned()]),
            minimum_severity: None,
            architecture: Some(&adapter.architecture),
            workspace_root: Some(ws.root()),
            page: &page(50, 0, None),
        },
    )
    .unwrap();
    let doc_rule_items = doc_rule_only["items"].as_array().unwrap();
    assert!(
        !doc_rule_items.iter().any(|it| {
            it["message"]
                .as_str()
                .is_some_and(|m| m.contains("INV-CORE-LEAF") || m.contains("INV-AST-GIT"))
        }),
        "filtered doc_integrity must not false-positive enforced invariants as orphans: {doc_rule_only}"
    );
    assert!(doc_rule_only["summary"]["by_category"].get("docs").is_none());

    // [linked]/... paths are non-glob targets and enforce PATH_NOT_INDEXED fail-closed.
    let linked_missing = run_lint(
        &conn,
        &LintRequest {
            target: Some("[repo]/missing.rs"),
            paths: None,
            categories: None,
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &page(10, 0, None),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(linked_missing.contains("PATH_NOT_INDEXED"), "{linked_missing}");

    // must_implement must still enforce `to.zones` even when `to.symbols` matches in another zone.
    let mut strict_zone_arch = adapter.architecture.clone();
    strict_zone_arch.rules.push(acdd_core::lint::ArchRuleSpec {
        id: "INV-MUST-IMPL-ZONE".to_owned(),
        invariant: Some("INV-MUST-IMPL-ZONE".to_owned()),
        severity: LintSeverity::Error,
        kind: acdd_core::lint::ArchRuleKind::MustImplement,
        from: Some(acdd_core::lint::ZoneSelector {
            paths: vec!["src/providers/**".to_owned()],
            ..Default::default()
        }),
        to: Some(acdd_core::lint::ZoneSelector {
            zones: vec!["core".to_owned()],
            symbols: vec!["DomainTrait".to_owned()],
            ..Default::default()
        }),
        owner_paths: Vec::new(),
        entrypoints: Vec::new(),
        edge_kinds: Vec::new(),
        confidences: Vec::new(),
        ast_pattern: None,
        zones: Vec::new(),
        depth: None,
        include_tests: false,
        message: None,
    });
    let zone_res = run_lint(
        &conn,
        &LintRequest {
            target: Some("src/providers/impls.rs"),
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: Some(&["INV-MUST-IMPL-ZONE".to_owned()]),
            minimum_severity: None,
            architecture: Some(&strict_zone_arch),
            workspace_root: Some(ws.root()),
            page: &page(50, 0, None),
        },
    )
    .unwrap();
    assert!(
        zone_res["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|it| it["symbol"].as_str().is_some_and(|s| s.ends_with("GoodProvider"))),
        "to.zones must not be bypassed by to.symbols match: {zone_res}"
    );

    let missing_err = run_lint(
        &conn,
        &LintRequest {
            target: Some("does/not/exist.rs"),
            paths: None,
            categories: None,
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &page(10, 0, None),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(missing_err.contains("PATH_NOT_INDEXED"), "{missing_err}");

    let p1 = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(LintCategory::ALL),
            rule_ids: None,
            minimum_severity: None,
            architecture: Some(&adapter.architecture),
            workspace_root: Some(ws.root()),
            page: &page(2, 0, None),
        },
    )
    .unwrap();
    assert_eq!(p1["has_more"], true);
    let p2 = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(LintCategory::ALL),
            rule_ids: None,
            minimum_severity: None,
            architecture: Some(&adapter.architecture),
            workspace_root: Some(ws.root()),
            page: &page(2, 2, p1["generation"].as_str()),
        },
    )
    .unwrap();
    assert_ne!(p1["items"], p2["items"]);
}

#[test]
fn delegated_hygiene_syntax_and_cycles_match_unified_lint() {
    let ws = Workspace::new();
    ws.write(
        "src/lib.rs",
        r#"pub mod a;
pub mod b;
pub fn dup_alpha() -> i32 {
    let x = 1;
    let y = 2;
    let z = 3;
    let w = 4;
    x + y + z + w
}
pub fn dup_beta() -> i32 {
    let x = 1;
    let y = 2;
    let z = 3;
    let w = 4;
    x + y + z + w
}
fn dead_helper() -> i32 {
    crate::missing::ghost_call();
    99
}
"#,
    );
    ws.write("src/a.rs", "use crate::b;\npub fn fa() { b::fb(); }\n");
    ws.write("src/b.rs", "use crate::a;\npub fn fb() { a::fa(); }\n");
    ws.write("src/broken.rs", "pub fn broken_syntax( {\n");
    ws.write(
        "docs/notes.md",
        "# Notes\nSee [`MissingSym`](../src/lib.rs#L1-L2).\n",
    );

    ws.build();
    let conn = ws.open();
    let opts = page(50, 0, None);

    let all = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(LintCategory::ALL),
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items: &[Value] = all["items"].as_array().unwrap();
    assert!(items.iter().any(|v| v["category"] == "syntax" && v["path"] == "src/broken.rs"));
    assert!(items.iter().any(|v| v["category"] == "duplicates" && v["severity"] == "warn"));
    assert!(items.iter().any(|v| v["category"] == "dead_tails" && v["rule_id"] == "dead_tails.unreachable"));
    assert!(items.iter().any(|v| v["category"] == "dead_tails" && v["rule_id"] == "dead_tails.dangling_call" && v["severity"] == "error"));
    assert!(items.iter().any(|v| v["category"] == "cycles" && v["severity"] == "warn"));
    assert!(items.iter().any(|v| v["category"] == "docs" && v["rule_id"] == "docs.broken_symbol_claim"));

    // Backward-compatible delegated entrypoints remain consistent.
    let syn = syntax_paged(&conn, "", &opts).unwrap();
    assert_eq!(syn["diagnostics"]["total"], 1);
    let dups = duplicates_paged(&conn, None, None, None, Some(5), &opts).unwrap();
    assert!(dups["total"].as_u64().unwrap() >= 1);
    let dt = dead_tails_paged(&conn, None, None, &opts).unwrap();
    assert!(dt["summary"]["dead_tails_count"].as_u64().unwrap() >= 1);
    assert!(dt["summary"]["dangling_calls_count"].as_u64().unwrap() >= 1);
    let cyc = cycles::cycles(&conn, None).unwrap();
    assert!(!cyc.as_array().unwrap().is_empty());
    let analyzed_doc = analyze_paged(&conn, "docs/notes.md", Some(false), &opts).unwrap();
    assert_eq!(analyzed_doc["doc_diagnostics"]["total"], 1);
    let analyzed_ws = analyze_paged(&conn, "", Some(true), &opts).unwrap();
    assert!(analyzed_ws["cycles"]["total"].as_u64().unwrap() >= 1);
}
