use crate::common::Workspace;
use acdd_core::config::load_adapter;
use acdd_core::lint::{LintCategory, LintSeverity};
use acdd_core::response::{QueryOptions, ResponsePolicy};
use acdd_index::db::{
    lint::{enforced_invariant_ids, run_lint, syntax_paged, LintRequest},
    reader::{self, analyze_paged},
    traversal::{duplicates_paged, query_with_path_options, GraphQueryOptions, ImpactDirection},
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
    let plain = docs::extract_with_details_with_zones("docs/plain.md", "# Plain\nText.\n", 0.0, &[], None).unwrap();
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
    let delta_report = pipeline::delta_with_selection(
        ws.root(),
        &delta_db,
        &[PathBuf::from("docs/arch.md")],
        None,
        Some(&cache_dir),
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
- INV-IMPL-FILTER: [`GoodProvider`](../src/providers/impls.rs)
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
    // INV-IMPL-FILTER links [GoodProvider](src/providers/impls.rs) without line range; impl exclusion resolves uniquely to struct GoodProvider.
    assert!(!items.iter().any(|it| {
        it["invariant"].as_str() == Some("INV-IMPL-FILTER")
            || it["symbol"].as_str().is_some_and(|s| s.contains("GoodProvider"))
                && matches!(
                    it["rule_id"].as_str(),
                    Some("docs.orphan_invariant" | "docs.ambiguous_symbol_link" | "docs.broken_symbol_link")
                )
    }), "INV-IMPL-FILTER should resolve cleanly without impl ambiguity: {items:?}");
    let governs_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges e JOIN coverage_evidence v ON v.evidence_id = e.evidence_id WHERE e.kind = 'governs' AND v.evidence = 'GoodProvider'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(governs_count, 1, "expected unique governs edge for GoodProvider");
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
        "# Notes\nSee [`lib.rs`](../src/lib.rs), [`notes.md`](notes.md), and [`MissingSym`](../src/lib.rs#L1-L2).\n",
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
    let dt = query_with_path_options(
        &conn,
        "dead_tails",
        None,
        None,
        &GraphQueryOptions {
            depth: 1,
            direction: ImpactDirection::Inbound,
            coverage: Default::default(),
            page: &opts,
        },
    )
    .unwrap();
    assert!(dt["summary"]["dead_tails_count"].as_u64().unwrap() >= 1);
    assert!(dt["summary"]["dangling_calls_count"].as_u64().unwrap() >= 1);
    let analyzed_doc = analyze_paged(&conn, "docs/notes.md", Some(false), &opts).unwrap();
    assert_eq!(analyzed_doc["doc_diagnostics"]["total"], 1);
    let analyzed_ws = analyze_paged(&conn, "", Some(true), &opts).unwrap();
    assert!(analyzed_ws["cycles"]["total"].as_u64().unwrap() >= 1);
}

#[cfg(feature = "lang-rust")]
#[test]
fn ast_clone_coalescing_and_import_cycle_separation_contract_matrix() {
    let ws = Workspace::new();
    // 1. Closing-brace blocks and wrapped single-statement calls must produce 0 duplicates.
    // 2. 8-statement shared block across distinct functions must coalesce into 1 maximal span.
    // 3. 1-node self-recursion and 2-node call-only recursion must produce 0 cycles.scc warnings,
    //    while 2-node module import SCC emits 1 cycles.scc warning.
    ws.write(
        "src/lib.rs",
        r#"pub mod imp_a;
pub mod imp_b;

pub fn braces_alpha() {
    if true {
        if true {
            if true {
                if true {
                    let _ = 1;
                }
            }
        }
    }
}

pub fn braces_beta() {
    if false {
        if false {
            if false {
                if false {
                    let _ = 2;
                }
            }
        }
    }
}

pub fn wrapped_alpha(a: i32, b: i32, c: i32, d: i32) -> i32 {
    helper_sum(
        a, // first
        b, // second
        c, // third
        d, // fourth
    )
}

pub fn wrapped_beta(a: i32, b: i32, c: i32, d: i32) -> i32 {
    helper_sum(
        a,
        b,
        c,
        d,
    )
}

fn helper_sum(a: i32, b: i32, c: i32, d: i32) -> i32 { a + b + c + d }

pub fn clone_host_one(v: i32) -> i32 {
    let preamble = v + 100;
    let s1 = preamble + 1;
    let s2 = s1 + 2;
    let s3 = s2 + 3;
    let s4 = s3 + 4;
    let s5 = s4 + 5;
    let s6 = s5 + 6;
    let s7 = s6 + 7;
    let s8 = s7 + 8;
    s8 + 11
}

pub fn clone_host_two(v: i32) -> i32 {
    let other_preamble = v + 200;
    let s1 = preamble + 1;
    let s2 = s1 + 2;
    let s3 = s2 + 3;
    let s4 = s3 + 4;
    let s5 = s4 + 5;
    let s6 = s5 + 6;
    let s7 = s6 + 7;
    let s8 = s7 + 8;
    s8 + 22
}

pub fn self_recursive(n: u32) -> u32 {
    if n == 0 { 0 } else { self_recursive(n - 1) }
}

pub fn mutual_ping(n: u32) -> u32 {
    if n == 0 { 0 } else { mutual_pong(n - 1) }
}

pub fn mutual_pong(n: u32) -> u32 {
    if n == 0 { 0 } else { mutual_ping(n - 1) }
}
"#,
    );
    ws.write("src/imp_a.rs", "use crate::imp_b;\npub fn fa() { imp_b::fb(); }\n");
    ws.write("src/imp_b.rs", "use crate::imp_a;\npub fn fb() { imp_a::fa(); }\n");

    ws.build();
    let conn = ws.open();
    let opts = page(100, 0, None);

    let dup_res = duplicates_paged(&conn, None, None, None, Some(5), &opts).unwrap();
    let dup_items = dup_res["items"].as_array().unwrap();
    assert_eq!(
        dup_items.len(),
        1,
        "closing braces & wrapped single-stmt calls must produce 0 duplicates, and 8-stmt clone must coalesce into 1 group: {dup_res}"
    );
    assert_eq!(dup_items[0]["match_type"], "block");
    assert_eq!(dup_items[0]["line_count"], 8);

    let lint_res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::Duplicates, LintCategory::Cycles]),
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items = lint_res["items"].as_array().unwrap();
    let cycle_items: Vec<&Value> = items.iter().filter(|v| v["category"] == "cycles").collect();
    assert_eq!(
        cycle_items.len(),
        1,
        "only the multi-node module import SCC should emit cycles.scc, never self/mutual call recursion: {cycle_items:?}"
    );
    assert_eq!(cycle_items[0]["rule_id"], "cycles.scc");
    assert!(
        cycle_items[0]["symbol"]
            .as_str()
            .is_some_and(|s| s.starts_with("module:src/imp_")),
        "{cycle_items:?}"
    );

    // 4. File-level `forbid_cycles` with `edge_kinds: [imports]` enforces `error` severity on
    //    same-directory import SCCs while allowing ordinary self/mutual call recursion.
    let arch_cfg: acdd_core::lint::ArchitectureConfig = serde_yaml::from_str(
        r#"
rules:
  - id: ARCH-ACYCLIC-SAME-DIR-IMPORTS
    severity: error
    kind: forbid_cycles
    from:
      paths: ["src/**"]
    edge_kinds: [imports]
"#,
    )
    .unwrap();
    let arch_res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: None,
            minimum_severity: Some(LintSeverity::Error),
            architecture: Some(&arch_cfg),
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let arch_items = arch_res["items"].as_array().unwrap();
    assert_eq!(
        arch_items.len(),
        1,
        "same-directory module import SCC must fail forbid_cycles error check without rejecting call recursion: {arch_items:?}"
    );
    assert_eq!(arch_items[0]["rule_id"], "ARCH-ACYCLIC-SAME-DIR-IMPORTS");
    assert_eq!(arch_items[0]["severity"], "error");
    assert!(
        arch_items[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("src/imp_a.rs") && m.contains("src/imp_b.rs")),
        "{arch_items:?}"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn structural_complexity_homonym_unused_zone_and_enforced_invariant_contract_matrix() {
    let ws = Workspace::new();
    ws.write(
        "acdd.yaml",
        r#"roots: [.]
architecture:
  complexity:
    severity: warn
    invariant: INV-BOUNDED-STRUCTURAL-COMPLEXITY
  zones:
    used_zone: ["src/clean/**"]
    unused_zone: ["src/unused/**"]
  rules:
    - id: INV-USED-ONLY
      invariant: INV-USED-ONLY
      severity: error
      kind: forbid_dependency
      from: { zones: ["used_zone"] }
      to: { external_symbols: ["rusqlite"] }
"#,
    );
    ws.write(
        "src/clean/router.rs",
        r#"pub enum Code { A, B, C, D, E, F }
pub fn flat_router(c: Code) -> i32 {
    match c {
        Code::A => 1,
        Code::B => 2,
        Code::C => 3,
        Code::D => 4,
        Code::E => 5,
        Code::F => 6,
    }
}
"#,
    );
    ws.write(
        "src/complex.rs",
        r#"pub fn deep_branching(items: &[i32]) -> i32 {
    let mut total = 0;
    for item in items {
        if *item > 0 {
            if *item % 2 == 0 {
                total += *item;
            }
        }
    }
    total
}
fn c1() {}
fn c2() {}
fn c3() {}
fn c4() {}
pub fn high_fanout_fn() {
    c1();
    c2();
    c3();
    c4();
}
pub fn overflow_budget_fn() {}
"#,
    );
    ws.write("src/hom_a.rs", "pub struct SharedType { pub a: i32 }\n");
    ws.write("src/hom_b.rs", "pub struct SharedType { pub b: i32 }\n");
    ws.write(
        "docs/arch.md",
        r#"---
title: Structural Bounds
doc_type: architecture
---
# Complexity
- INV-BOUNDED-STRUCTURAL-COMPLEXITY: Bounded complexity enforced via architecture.complexity.
- INV-USED-ONLY: Used zone rule.
"#,
    );

    ws.build();
    let conn = ws.open();

    // Verify syntax.fact_budget_exceeded via an in-memory SQLite fixture (< 1 ms, no sealed DB mutation).
    let mem_conn = rusqlite::Connection::open_in_memory().unwrap();
    mem_conn
        .execute_batch(&format!(
            "{}INSERT INTO metadata(key,value) VALUES ('output_root','mem-gen');
             INSERT INTO path_dictionary(path_id,path) VALUES (1,'src/complex.rs');
             INSERT INTO files(path,status,digest,size,language,is_test,generated,path_hash)
             VALUES ('src/complex.rs','ok','d',1,'rust',0,0,1);
             INSERT INTO nodes(node_id,id,kind,name,qualname,path_id,line,end_line,is_test,language,generated,details,node_hash,owner_path_id)
             VALUES (1,'rs:src/complex.rs:1:overflow_budget_fn','function','overflow_budget_fn','overflow_budget_fn',1,1,2,0,'rust',0,'{{\"fact_limit_exceeded\":true}}',100,1);",
            acdd_index::db::schema::SCHEMA_DDL
        ))
        .unwrap();
    let mem_cfg = acdd_core::lint::ArchitectureConfig {
        complexity: acdd_core::lint::ComplexityLimits {
            invariant: Some("INV-BOUNDED-STRUCTURAL-COMPLEXITY".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    let mem_lint = run_lint(
        &mem_conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::Syntax]),
            rule_ids: None,
            minimum_severity: None,
            architecture: Some(&mem_cfg),
            workspace_root: None,
            page: &page(20, 0, None),
        },
    )
    .unwrap();
    let mem_items = mem_lint["items"].as_array().unwrap();
    assert!(
        mem_items.iter().any(|v| {
            v["rule_id"] == "syntax.fact_budget_exceeded"
                && v["symbol"] == "overflow_budget_fn"
                && v["invariant"] == "INV-BOUNDED-STRUCTURAL-COMPLEXITY"
        }),
        "expected syntax.fact_budget_exceeded on overflow_budget_fn: {mem_lint}"
    );

    let adapter = load_adapter(ws.root(), None).unwrap();
    let enforced = enforced_invariant_ids(&conn, &adapter.architecture.rules).unwrap();
    assert!(
        enforced.contains("INV-BOUNDED-STRUCTURAL-COMPLEXITY"),
        "enforced_invariant_ids must include architecture.complexity.invariant: {enforced:?}"
    );
    assert!(enforced.contains("INV-USED-ONLY"));

    let opts = page(100, 0, None);
    let full = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: None,
            rule_ids: None,
            minimum_severity: None,
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items = full["items"].as_array().unwrap();
    let has = |rule_id: &str, sym_substr: &str| {
        items.iter().any(|v| {
            v["rule_id"] == rule_id
                && (sym_substr.is_empty()
                    || v["symbol"]
                        .as_str()
                        .is_some_and(|s| s.contains(sym_substr)))
        })
    };

    let deep_details: Value = conn
        .query_row(
            "SELECT details FROM nodes WHERE name = 'deep_branching'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).unwrap())
        .unwrap();
    assert!(
        deep_details["cognitive_complexity"].as_u64().unwrap_or(0) > 4,
        "deep_branching must record cognitive_complexity in node.details: {deep_details}"
    );
    assert!(
        deep_details["max_nesting_depth"].as_u64().unwrap_or(0) > 2,
        "deep_branching must record max_nesting_depth in node.details: {deep_details}"
    );
    let router_details: Value = conn
        .query_row(
            "SELECT details FROM nodes WHERE name = 'flat_router'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map(|raw| serde_json::from_str(&raw).unwrap())
        .unwrap();
    assert!(
        router_details["cognitive_complexity"].as_u64().unwrap_or(0) <= 4,
        "flat_router must not inflate cognitive_complexity in node.details: {router_details}"
    );
    assert!(
        router_details["max_nesting_depth"].as_u64().unwrap_or(0) <= 2,
        "flat_router must not inflate max_nesting_depth in node.details: {router_details}"
    );
    assert!(!has("syntax.cognitive_complexity", ""), "{items:?}");
    assert!(!has("syntax.max_nesting_depth", ""), "{items:?}");
    assert!(!has("syntax.high_fanout", ""), "{items:?}");
    assert!(has("syntax.intra_crate_type_homonym", "SharedType"), "{items:?}");

    let matching: Vec<&Value> = items
        .iter()
        .filter(|v| v["rule_id"] == "syntax.intra_crate_type_homonym")
        .collect();
    assert!(!matching.is_empty(), "missing syntax.intra_crate_type_homonym");
    assert!(
        matching
            .iter()
            .all(|v| v["invariant"] == "INV-BOUNDED-STRUCTURAL-COMPLEXITY"),
        "syntax.intra_crate_type_homonym must bind invariant: {matching:?}"
    );

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "docs.unused_arch_zone"
                && v["severity"] == "warn"
                && v["target"] == "unused_zone"
        }),
        "expected docs.unused_arch_zone for unused_zone: {items:?}"
    );
    assert!(
        !items
            .iter()
            .any(|v| v["rule_id"] == "docs.unused_arch_zone" && v["target"] == "used_zone"),
        "used_zone must not emit docs.unused_arch_zone: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["rule_id"] == "docs.orphan_invariant"
                && v["invariant"] == "INV-BOUNDED-STRUCTURAL-COMPLEXITY"
        }),
        "INV-BOUNDED-STRUCTURAL-COMPLEXITY must not be flagged as orphan: {items:?}"
    );
}

#[cfg(feature = "lang-rust")]
#[test]
fn dead_tails_unused_public_api_enforces_cross_module_non_test_usage_matrix() {
    let ws = Workspace::new();
    ws.write(
        "Cargo.toml",
        r#"[package]
name = "sample-app"
version = "0.1.0"
edition = "2021"

[lib]
name = "sample_app"
path = "src/lib.rs"
"#,
    );
    ws.write(
        "build.rs",
        r#"#[path = "build_helper.rs"]
mod build_helper;

fn main() {
    build_helper::emit();
}
"#,
    );
    ws.write(
        "build_helper.rs",
        r#"pub fn emit() {}
"#,
    );
    ws.write(
        "src/lib.rs",
        r#"pub mod inner;
pub mod other;
pub mod caller;
pub mod glob_used;
pub mod glob_unused;
#[cfg(test)]
pub mod helper;
pub mod harness;
#[cfg(test)]
pub(crate) mod dual_cfg;
#[cfg(not(test))]
mod dual_cfg;
pub mod a;
pub mod reexport_a;
pub mod reexport_b;
pub mod reexport_c;
pub mod reexport_named_b;
pub mod dead_glob_mod;
pub mod data_only;

pub use inner::unused_reexport;
pub use inner::test_only_reexport;
pub use inner::used_reexport;
pub use inner::direct_submod_masked;
pub use inner::main_reexport;
pub use inner::unrelated_unresolved_reexport;
pub use inner::test_only_unresolved_reexport;
pub use missing_mod::mixed_unresolved_reexport;
pub use missing_mod::helper_unresolved_reexport;
pub use missing_mod::data_only_unresolved;
pub use glob_used::*;
pub use glob_unused::*;

#[derive(serde::Serialize)]
pub struct PublicDto {
    pub field: i32,
}

#[derive(Debug, Clone, Default)]
pub struct NonWireDeadDto {
    pub field: i32,
}

pub fn main_entrypoint() {}
pub fn lib_unused() {}

#[cfg(test)]
mod test_imports {
    use crate::used_reexport;
}
"#,
    );
    ws.write(
        "src/data_only.rs",
        r#"pub struct DataDto {
    pub value: i32,
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let _ = crate::data_only_unresolved();
    }
}
"#,
    );
    ws.write(
        "src/helper.rs",
        r#"pub fn test_helper_only() -> i32 {
    let _ = sample_app::helper_unresolved_reexport();
    crate::inner::test_only_from_cfg_test_mod()
}
"#,
    );
    ws.write(
        "src/harness.rs",
        r#"pub fn prod_harness_used() -> i32 { 101 }
pub fn prod_harness_unused() -> i32 { 102 }
"#,
    );
    ws.write(
        "src/dual_cfg.rs",
        r#"pub fn dual_cfg_used() -> i32 { 103 }
pub fn dual_cfg_unused() -> i32 { 104 }
"#,
    );
    ws.write(
        "src/a/mod.rs",
        r#"pub mod b;
pub mod child;

pub use b::submod_reexport;
pub use b::used_via_a;
pub use b::used_via_alias;
pub use b::super_submod_masked;
pub use b::super_used_via_a;
pub use missing_sub::a_test_unresolved;
"#,
    );
    ws.write(
        "src/a/b.rs",
        r#"pub fn submod_reexport() -> i32 { 110 }
pub fn used_via_a() -> i32 { 111 }
pub fn used_via_alias() -> i32 { 114 }
pub fn super_submod_masked() -> i32 { 112 }
pub fn super_used_via_a() -> i32 { 113 }
"#,
    );
    ws.write(
        "src/a/child.rs",
        r#"pub fn child_entry() -> i32 {
    super::super_used_via_a() + super::b::super_submod_masked()
}
"#,
    );
    ws.write(
        "src/reexport_a.rs",
        r#"pub use crate::reexport_b::*;
pub use crate::reexport_named_b::direct_foo;
"#,
    );
    ws.write(
        "src/reexport_b.rs",
        r#"pub use crate::reexport_c::via_glob_named;
pub use crate::reexport_c::unused_in_c;
"#,
    );
    ws.write(
        "src/reexport_c.rs",
        r#"pub fn via_glob_named() -> i32 { 120 }
pub fn unused_in_c() -> i32 { 121 }
"#,
    );
    ws.write(
        "src/reexport_named_b.rs",
        r#"pub fn direct_foo() -> i32 { 122 }
pub use crate::dead_glob_mod::*;
"#,
    );
    ws.write(
        "src/dead_glob_mod.rs",
        r#"pub fn dead_glob_fn() -> i32 { 123 }
"#,
    );
    ws.write(
        "src/glob_used.rs",
        r#"pub fn glob_live() -> i32 { 10 }
pub fn glob_dead() -> i32 { 11 }
"#,
    );
    ws.write(
        "src/glob_unused.rs",
        r#"pub fn never_used_via_glob() -> i32 { 12 }
"#,
    );
    ws.write(
        "src/other.rs",
        r#"pub fn shadowed_local_fn() -> i32 { 20 }
pub fn macro_arg_fn() -> i32 { 21 }
pub fn macro_path_fn() -> i32 { 22 }
"#,
    );
    ws.write(
        "src/inner.rs",
        r#"pub fn used_reexport() -> i32 { 1 }
pub fn unused_reexport() -> i32 { 2 }
pub fn test_only_reexport() -> i32 { 3 }
pub fn used_direct_pub() -> i32 { 4 }
pub fn unused_pub_fn() -> i32 { 5 }
pub fn test_only_pub_fn() -> i32 { 6 }
pub fn cfg_all_test_only_fn() -> i32 { 7 }
pub fn cfg_any_test_only_fn() -> i32 { 8 }
pub fn shadowed_local_fn() -> i32 { 9 }
pub fn direct_submod_masked() -> i32 { 10 }
pub fn main_reexport() -> i32 { 11 }
pub fn unrelated_unresolved_reexport() -> i32 { 12 }
pub fn test_only_unresolved_reexport() -> i32 { 13 }
pub fn test_only_from_cfg_test_mod() -> i32 { 14 }

#[cfg(test)]
mod tests {
    use crate::test_only_reexport;
    #[test]
    fn t() {
        let _brace_trap = "}}}";
        let _ = test_only_reexport();
        let _ = super::test_only_pub_fn();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    #[test]
    fn l() {
        let _ = super::cfg_all_test_only_fn();
    }
}

#[cfg(any(test, feature = "nonexistent-test-flag"))]
mod any_tests {
    #[test]
    fn a() {
        let _ = super::cfg_any_test_only_fn();
    }
}
"#,
    );
    ws.write(
        "src/caller.rs",
        r#"use crate::used_reexport;
use crate::glob_live;
use crate::inner::used_direct_pub;
use crate::other::shadowed_local_fn as other_shadowed;
use crate::a::{self, self as aliased_a};

struct Helper;
impl Helper {
    fn shadowed_local_fn(&self) -> i32 { 0 }
}

fn shadowed_local_fn() -> i32 { 0 }

macro_rules! invoke_helper {
    ($e:expr) => {
        $e
    };
}

pub fn prod_entry() -> i32 {
    let h = Helper;
    used_reexport()
        + glob_live()
        + used_direct_pub()
        + other_shadowed()
        + shadowed_local_fn()
        + h.shadowed_local_fn()
        + invoke_helper!(crate::other::macro_arg_fn())
        + crate::inner::direct_submod_masked()
        + a::used_via_a()
        + aliased_a::used_via_alias()
        + aliased_a::b::submod_reexport()
        + a::b::submod_reexport()
        + crate::a::b::submod_reexport()
        + crate::a::child::child_entry()
        + crate::harness::prod_harness_used()
        + crate::dual_cfg::dual_cfg_used()
        + crate::reexport_a::via_glob_named()
        + crate::reexport_a::direct_foo()
        + unknown_crate::unrelated_unresolved_reexport()
}

#[cfg(test)]
mod tests {
    #[test]
    fn mixed_file_unresolved_test_call() {
        let _ = crate::mixed_unresolved_reexport();
        let _ = sample_app::mixed_unresolved_reexport();
        let _ = a::a_test_unresolved();
        let _ = crate::unknown_test_mod::test_only_unresolved_reexport();
    }
}
"#,
    );
    ws.write(
        "src/main.rs",
        r#"macro_rules! pass_path_helper {
    ($f:path) => {
        $f()
    };
}

fn main() {
    let _ = sample_app::caller::prod_entry();
    let _ = pass_path_helper!(sample_app::other::macro_path_fn);
    sample_app::main_entrypoint();
    let _ = sample_app::main_reexport();
}
"#,
    );

    ws.build();
    let conn = ws.open();
    let opts = page(100, 0, None);

    let default_disabled = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::DeadTails]),
            rule_ids: Some(&["dead_tails.unused_public_api".to_owned()]),
            minimum_severity: Some(LintSeverity::Error),
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    assert!(
        default_disabled["items"].as_array().unwrap().is_empty(),
        "dead_tails.unused_public_api must be opt-in (false by default): {default_disabled:?}"
    );

    let opt_in_arch = acdd_core::lint::ArchitectureConfig {
        unused_public_api: true,
        ..Default::default()
    };
    let res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::DeadTails]),
            rule_ids: Some(&["dead_tails.unused_public_api".to_owned()]),
            minimum_severity: Some(LintSeverity::Error),
            architecture: Some(&opt_in_arch),
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items = res["items"].as_array().unwrap();
    let symbols: Vec<&str> = items
        .iter()
        .filter_map(|v| v["symbol"].as_str())
        .collect();

    let expect_flagged = [
        ("unused_reexport", Some("src/lib.rs")),
        ("test_only_reexport", Some("src/lib.rs")),
        ("direct_submod_masked", Some("src/lib.rs")),
        ("unrelated_unresolved_reexport", Some("src/lib.rs")),
        ("test_only_unresolved_reexport", Some("src/lib.rs")),
        ("mixed_unresolved_reexport", Some("src/lib.rs")),
        ("helper_unresolved_reexport", Some("src/lib.rs")),
        ("data_only_unresolved", Some("src/lib.rs")),
        ("DataDto", Some("src/data_only.rs")),
        ("submod_reexport", Some("src/a/mod.rs")),
        ("a_test_unresolved", Some("src/a/mod.rs")),
        ("super_submod_masked", Some("src/a/mod.rs")),
        ("unused_pub_fn", Some("src/inner.rs")),
        ("test_only_pub_fn", Some("src/inner.rs")),
        ("cfg_all_test_only_fn", Some("src/inner.rs")),
        ("cfg_any_test_only_fn", Some("src/inner.rs")),
        ("shadowed_local_fn", Some("src/inner.rs")),
        ("test_only_from_cfg_test_mod", Some("src/inner.rs")),
        ("glob_dead", Some("src/glob_used.rs")),
        ("never_used_via_glob", Some("src/glob_unused.rs")),
        ("NonWireDeadDto", Some("src/lib.rs")),
        ("lib_unused", Some("src/lib.rs")),
        ("prod_harness_unused", Some("src/harness.rs")),
        ("dual_cfg_unused", Some("src/dual_cfg.rs")),
        ("unused_in_c", Some("src/reexport_b.rs")),
    ];
    for (name, expected_path) in expect_flagged {
        assert!(
            items.iter().any(|v| {
                v["symbol"]
                    .as_str()
                    .and_then(|s| s.rsplit('.').next())
                    == Some(name)
                    && expected_path.is_none_or(|p| v["path"] == p)
            }),
            "expected dead_tails.unused_public_api for '{name}' at {expected_path:?}: {items:?}"
        );
    }
    for (expected_path, expected_glob) in [
        ("src/lib.rs", "glob_unused::*"),
        ("src/reexport_named_b.rs", "dead_glob_mod::*"),
    ] {
        assert!(
            items.iter().any(|v| {
                v["path"] == expected_path
                    && v["message"]
                        .as_str()
                        .is_some_and(|m| m.contains(expected_glob))
            }),
            "expected unused glob pub use violation for {expected_glob} at {expected_path}: {items:?}"
        );
    }
    assert!(
        !items.iter().any(|v| {
            v["path"] == "src/lib.rs"
                && v["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("glob_used::*"))
        }),
        "did not expect unused glob pub use violation for glob_used::*: {items:?}"
    );
    for (name, disallowed_path) in [
        ("used_reexport", None),
        ("used_direct_pub", None),
        ("main_reexport", Some("src/lib.rs")),
        ("used_via_a", Some("src/a/mod.rs")),
        ("used_via_alias", Some("src/a/mod.rs")),
        ("super_used_via_a", Some("src/a/mod.rs")),
        ("prod_harness_used", Some("src/harness.rs")),
        ("dual_cfg_used", Some("src/dual_cfg.rs")),
        ("test_helper_only", Some("src/helper.rs")),
        ("via_glob_named", Some("src/reexport_b.rs")),
        ("direct_foo", Some("src/reexport_a.rs")),
        ("glob_live", None),
        ("prod_entry", None),
        ("PublicDto", None),
        ("main", None),
        ("main_entrypoint", None),
        ("emit", Some("build_helper.rs")),
        ("macro_arg_fn", Some("src/other.rs")),
        ("macro_path_fn", Some("src/other.rs")),
        ("shadowed_local_fn", Some("src/other.rs")),
    ] {
        assert!(
            !items.iter().any(|v| {
                v["symbol"]
                    .as_str()
                    .and_then(|s| s.rsplit('.').next())
                    == Some(name)
                    && disallowed_path.is_none_or(|p| v["path"] == p)
            }),
            "did not expect dead_tails.unused_public_api for '{name}' at {disallowed_path:?}: {items:?}"
        );
    }
    assert!(
        items.iter().all(|v| v["severity"] == "error"),
        "dead_tails.unused_public_api must emit error severity: {items:?}"
    );

    let warn_res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::DeadTails]),
            rule_ids: None,
            minimum_severity: Some(LintSeverity::Warn),
            architecture: Some(&opt_in_arch),
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let warn_items = warn_res["items"].as_array().unwrap();
    assert!(
        !warn_items
            .iter()
            .any(|v| v["path"] == "src/helper.rs"),
        "#[cfg(test)] pub mod helper in src/lib.rs must mark src/helper.rs test-only: {warn_items:?}"
    );
    let _ = symbols;
}

#[test]
fn soundness_batch_ast_pattern_possible_calls_externals_cycles_and_must_implement_matrix() {
    let ws = Workspace::new();
    ws.write(
        "acdd.yaml",
        r##"roots:
  - src
  - py
architecture:
  zones:
    a_broad: ["src/**"]
    z_nested: ["src/nested/**"]
    other: ["src/other/**"]
    api: ["src/api/**", "py/api/**"]
    domain: ["src/domain/**", "py/domain/**"]
  rules:
    - id: AST-CMD-QUAL
      severity: error
      kind: ast_pattern
      from: { zones: ["a_broad"] }
      ast_pattern: "std::process::Command::new($PROGRAM)"
    - id: AST-SQL-QUAL
      severity: error
      kind: ast_pattern
      from: { zones: ["a_broad"] }
      ast_pattern: "rusqlite::Connection::open($PATH)"
    - id: AST-ALLOW-INNER
      severity: error
      kind: ast_pattern
      from: { zones: ["a_broad"] }
      ast_pattern: "#![allow(dead_code)]"
    - id: AST-EXPECT-ATTR
      severity: error
      kind: ast_pattern
      from: { zones: ["a_broad"] }
      ast_pattern: "#[expect(dead_code)]"
    - id: AST-EXPECT-INNER
      severity: error
      kind: ast_pattern
      from: { zones: ["a_broad"] }
      ast_pattern: "#![expect(dead_code)]"
    - id: ALLOW-ONLY-SERDE
      severity: error
      kind: allow_only_dependencies
      from: { zones: ["domain"] }
      to: { zones: ["api"], external_symbols: ["serde"] }
    - id: POSSIBLE-EDGE-FORBID
      severity: error
      kind: forbid_dependency
      from: { zones: ["api"] }
      to: { zones: ["domain"] }
    - id: CYCLE-ZONE-SPECIFICITY
      severity: error
      kind: forbid_cycles
      zones: ["a_broad", "z_nested", "other"]
    - id: MUST-IMPL-HANDLER
      severity: error
      kind: must_implement
      from: { zones: ["api"] }
      to: { symbols: ["Handler"] }
"##,
    );
    ws.write(
        "src/lib.rs",
        "pub mod ast_hits;\npub mod clean_with_test;\npub mod nested;\npub mod other;\npub mod api;\npub mod domain;\n",
    );
    ws.write(
        "src/ast_hits.rs",
        r#"#![allow(dead_code)]
#![expect(dead_code)]

#[expect(dead_code)]
fn suppressed() {}

pub fn spawn_and_open() {
    let _ = std::process::Command::new("echo");
    let _ = rusqlite::Connection::open("db.sqlite");
}
"#,
    );
    ws.write(
        "src/clean_with_test.rs",
        r#"pub fn prod_ok() -> usize { 1 }

#[cfg(test)]
mod tests {
    #[test]
    fn test_open() {
        let _ = rusqlite::Connection::open("test.sqlite");
    }
}
"#,
    );
    ws.write(
        "src/domain/mod.rs",
        "pub mod allowed;\npub mod disallowed;\n",
    );
    ws.write(
        "src/domain/allowed.rs",
        r#"use serde::Serialize;

pub fn target_a() {}
pub fn target_b() {}

pub trait DomainLocalTrait {
    fn local_op(&self);
}

pub struct DomainPortImpl;
pub struct AltPortImpl;
pub fn new_port() -> DomainPortImpl { DomainPortImpl }
pub fn new_alt_port() -> AltPortImpl { AltPortImpl }
impl crate::api::types::Port for DomainPortImpl {
    fn ping(&self) {}
}
impl DomainLocalTrait for DomainPortImpl {
    fn local_op(&self) {}
}
impl DomainPortImpl {
    pub fn inherent_secret(&self) {}
}
impl AltPortImpl {
    pub fn inherent_secret(&self) {}
}

#[derive(Serialize)]
pub struct Payload {
    pub x: i32,
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    #[test]
    fn allowed_test_dep() {
        let _ = Connection::open("t.sqlite");
    }
}
"#,
    );
    ws.write(
        "src/domain/disallowed.rs",
        r#"use tokio::runtime::Runtime;
pub fn bad_ext_user() {
    let _ = Runtime::new();
}

#[cfg(test)]
mod tests {
    use tokio::runtime::Runtime;
    #[test]
    fn mixed_test_tokio_usage() {
        let _ = Runtime::new();
    }
}
"#,
    );
    ws.write("src/nested/mod.rs", "pub mod a;\n");
    ws.write(
        "src/nested/a.rs",
        "use crate::other::b;\npub fn fa() { b::fb(); }\n",
    );
    ws.write("src/other/mod.rs", "pub mod b;\n");
    ws.write(
        "src/other/b.rs",
        "use crate::nested::a;\npub fn fb() { a::fa(); }\n",
    );
    ws.write("src/api/mod.rs", "pub mod types;\npub mod impls;\npub mod branch;\n");
    ws.write(
        "src/api/types.rs",
        r#"pub trait Handler {}
pub trait Port {
    fn ping(&self);
}

pub struct MissingStruct {
    pub id: i32,
}

pub enum MissingEnum {
    Alpha,
    Beta,
}

pub struct CrossFileOk;

struct PrivateSerdeHelper;

pub fn unrelated_helper() {
    external_crate::tick();
}
"#,
    );
    ws.write(
        "src/api/impls.rs",
        "use crate::api::types::{CrossFileOk, Handler};\nimpl Handler for CrossFileOk {}\n",
    );
    ws.write(
        "src/api/branch.rs",
        r#"pub fn via_port(port: &dyn crate::api::types::Port) {
    port.ping();
}

pub fn maybe_call(flag: bool) {
    let callee = if flag {
        crate::domain::allowed::target_a
    } else {
        crate::domain::allowed::target_b
    };
    callee();
}
"#,
    );
    ws.write(
        "py/api/types.py",
        "class PyPort:\n    def ping(self):\n        pass\n",
    );
    ws.write(
        "py/domain/allowed.py",
        "from py.api.types import PyPort\nclass PyPortImpl(PyPort):\n    def ping(self):\n        pass\n    def inherent_secret(self):\n        pass\nclass AltPyImpl:\n    def inherent_secret(self):\n        pass\n",
    );
    ws.write(
        "py/api/branch.py",
        "from py.domain.allowed import PyPortImpl, AltPyImpl\ndef maybe_inherent(condition):\n    if condition:\n        receiver = PyPortImpl()\n    else:\n        receiver = AltPyImpl()\n    receiver.inherent_secret()\n",
    );

    ws.build();
    let conn = ws.open();
    let opts = page(100, 0, None);
    let res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[LintCategory::Arch]),
            rule_ids: None,
            minimum_severity: Some(LintSeverity::Error),
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items = res["items"].as_array().unwrap();

    for rule_id in [
        "AST-CMD-QUAL",
        "AST-SQL-QUAL",
        "AST-ALLOW-INNER",
        "AST-EXPECT-ATTR",
        "AST-EXPECT-INNER",
    ] {
        assert!(
            items.iter().any(|v| v["rule_id"] == rule_id && v["path"] == "src/ast_hits.rs"),
            "expected batched ast_pattern rule {rule_id} hit in src/ast_hits.rs: {items:?}"
        );
        assert!(
            !items.iter().any(|v| v["rule_id"] == rule_id && v["path"] == "src/clean_with_test.rs"),
            "test-only code in clean_with_test.rs must not trigger {rule_id}: {items:?}"
        );
    }

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "ALLOW-ONLY-SERDE"
                && v["path"] == "src/domain/disallowed.rs"
                && v["line"].as_u64().is_some_and(|l| l <= 3)
                && v["message"].as_str().is_some_and(|m| m.contains("tokio"))
        }),
        "allow_only_dependencies must flag prod tokio in mixed prod+test file: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["rule_id"] == "ALLOW-ONLY-SERDE"
                && v["path"] == "src/domain/disallowed.rs"
                && v["line"].as_u64().is_some_and(|l| l >= 6)
        }),
        "test-only tokio lines inside mixed file must remain excluded: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["rule_id"] == "ALLOW-ONLY-SERDE" && v["path"] == "src/domain/allowed.rs"
        }),
        "allow_only_dependencies must permit serde and ignore #[cfg(test)] rusqlite: {items:?}"
    );

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "CYCLE-ZONE-SPECIFICITY"
                && v["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("other") && m.contains("z_nested"))
        }),
        "forbid_cycles must pick most-specific zone 'z_nested' over alphabetical 'a_broad': {items:?}"
    );

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "POSSIBLE-EDGE-FORBID" && v["path"] == "src/api/branch.rs"
        }),
        "forbid_dependency with default confidences must admit possible_calls/branch edges: {items:?}"
    );
    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "POSSIBLE-EDGE-FORBID"
                && v["path"] == "py/api/branch.py"
                && v["symbol"]
                    .as_str()
                    .is_some_and(|s| s.ends_with(".inherent_secret"))
        }),
        "inherent method on PyPortImpl (inheriting PyPort) must still be flagged on possible_calls from api->domain: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["rule_id"] == "POSSIBLE-EDGE-FORBID"
                && v["symbol"]
                    .as_str()
                    .is_some_and(|s| s.contains("DomainPortImpl") && s.ends_with(".ping"))
        }),
        "Dependency Inversion via allowed Port trait must not false-positive even when DomainPortImpl also implements DomainLocalTrait: {items:?}"
    );

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "MUST-IMPL-HANDLER"
                && v["symbol"].as_str().and_then(|s| s.rsplit('.').next()) == Some("MissingStruct")
        }),
        "must_implement must flag public struct MissingStruct: {items:?}"
    );
    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "MUST-IMPL-HANDLER"
                && v["symbol"].as_str().and_then(|s| s.rsplit('.').next()) == Some("MissingEnum")
        }),
        "must_implement must flag public enum MissingEnum: {items:?}"
    );
    for exempt in ["CrossFileOk", "PrivateSerdeHelper"] {
        assert!(
            !items.iter().any(|v| {
                v["rule_id"] == "MUST-IMPL-HANDLER"
                    && v["symbol"].as_str().and_then(|s| s.rsplit('.').next()) == Some(exempt)
            }),
            "must_implement must not flag cross-file impl or private struct '{exempt}': {items:?}"
        );
    }
}

#[test]
fn lint_hygiene_docs_homonym_duplicates_callbacks_and_cycles_matrix() {
    let ws = Workspace::new();
    ws.write(
        "acdd.yaml",
        r#"roots:
  - crates
docs:
  - docs
architecture:
  complexity:
    severity: error
    invariant: INV-BOUNDED-STRUCTURAL-COMPLEXITY
"#,
    );
    ws.write(
        "crates/pkg_a/Cargo.toml",
        "[package]\nname = \"pkg_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    ws.write(
        "crates/pkg_b/Cargo.toml",
        "[package]\nname = \"pkg_b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    ws.write(
        "crates/pkg_a/src/lib.rs",
        "pub mod m1;\npub mod m2;\npub mod cyc_a;\npub mod cyc_b;\n",
    );
    ws.write(
        "crates/pkg_a/src/m1.rs",
        r#"pub struct SharedCfg { pub a: i32 }
pub mod sub_one { pub struct LocalDup { pub x: i32 } }
pub mod sub_two { pub struct LocalDup { pub z: i32 } }

pub fn compute_alpha(input: i32) -> i32 {
    let base = input + 1;
    let s1 = base * 2;
    let s2 = s1 + 3;
    let s3 = s2 - 1;
    let s4 = s3 * 3;
    s4 + 10
}

pub fn near_alpha(v: i32) -> i32 {
    let a = v + 100;
    let b = a * 4;
    let c = b - 7;
    let d = c + 19;
    let e = d * 5;
    let f = e - 3;
    f + 42
}

#[cfg(test)]
mod tests {
    pub fn test_clone_one(x: i32) -> i32 {
        let base = x + 1;
        let s1 = base * 2;
        let s2 = s1 + 3;
        let s3 = s2 - 1;
        let s4 = s3 * 3;
        s4 + 10
    }
}
"#,
    );
    ws.write(
        "crates/pkg_a/src/m2.rs",
        r#"pub struct LocalDup { pub y: i32 }

pub fn compute_beta(val: i32) -> i32 {
    let seed = val + 1;
    let t1 = seed * 2;
    let t2 = t1 + 3;
    let t3 = t2 - 1;
    let t4 = t3 * 3;
    t4 + 10
}

pub fn near_beta(w: i32) -> i32 {
    let x = w + 100;
    let y = x * 4;
    let extra = y ^ 99;
    let z = y - 7;
    let u = z + 19;
    let p = u * 5;
    let q = p - 3;
    q + 42
}

#[cfg(test)]
mod tests {
    pub fn test_clone_two(y: i32) -> i32 {
        let seed = y + 1;
        let t1 = seed * 2;
        let t2 = t1 + 3;
        let t3 = t2 - 1;
        let t4 = t3 * 3;
        t4 + 10
    }
}
"#,
    );
    ws.write(
        "crates/pkg_b/src/lib.rs",
        "pub struct SharedCfg { pub b: i32 }\n",
    );
    ws.write(
        "crates/pkg_a/src/cyc_a.rs",
        "#[cfg(test)]\nmod tests {\n    use crate::cyc_b;\n    pub fn ta() { cyc_b::tests::tb(); }\n}\n",
    );
    ws.write(
        "crates/pkg_a/src/cyc_b.rs",
        "#[cfg(test)]\nmod tests {\n    use crate::cyc_a;\n    pub fn tb() { cyc_a::tests::ta(); }\n}\n",
    );
    ws.write(
        "crates/pkg_a/src/reexports/mod.rs",
        r#"mod reexport_fn;
mod builder_def;
mod builder_impl;
pub(crate) use reexport_fn::reexport_fn;
pub(in crate) use builder_def::Builder;
impl builder_def::Builder {
    pub(in crate) fn finish_step(self) {}
}
"#,
    );
    ws.write(
        "crates/pkg_a/src/reexports/reexport_fn.rs",
        "pub(crate) fn reexport_fn() {}\n",
    );
    ws.write(
        "crates/pkg_a/src/reexports/builder_def.rs",
        r#"pub(crate) struct Builder;
impl Builder {
    pub(crate) fn with_step(self) -> Self { self }
}
"#,
    );
    ws.write(
        "crates/pkg_a/src/reexports/builder_impl.rs",
        r#"use super::builder_def::Builder;
impl Builder {
    pub(crate) fn build() -> Self { Self }
}
"#,
    );
    ws.write(
        "crates/pkg_a/src/main.rs",
        r#"pub mod reexports;
pub struct UnknownApi;
pub fn on_event(v: i32) -> i32 { v + 1 }
pub fn register(_f: fn(i32) -> i32) {}
fn comb_cb(v: i32) -> i32 { v + 2 }
fn depth() -> i32 { 2 }
fn schemars_cb() -> i32 { 3 }
fn tokens(s: &str) -> &str { s }
fn macro_guard_fn(v: i32) -> bool { v > 0 }

#[derive(Deserialize, JsonSchema)]
pub struct AttrHolder {
    #[serde(default = "depth")]
    pub depth: i32,
    #[schemars(schema_with = "schemars_cb")]
    pub extra: i32,
}

pub struct Holder;
impl Holder {
    pub fn custom_hook(&self) {}
}

pub fn main_wire(ext: UnknownApi) {
    let cb = on_event;
    register(cb);
    ext.custom_hook();
    let _ = [1].into_iter().map(comb_cb).count();
    let tokens = tokens("SELECT 1");
    let _ = tokens;
    let _ = matches!(Some(1), Some(v) if macro_guard_fn(v));
    let _ = core::mem::size_of::<AttrHolder>();
    reexports::reexport_fn();
    let builder = reexports::Builder::build().with_step().finish_step();
    let _ = builder;
}

fn main() {
    main_wire(UnknownApi);
}
"#,
    );
    ws.write(
        "docs/arch.md",
        r#"---
title: Arch Spec
doc_type: architecture
---
# Spec

- Broken link: [missing](missing.md)
- Out of range: [`SharedCfg`](../crates/pkg_b/src/lib.rs#L1-L99)
- Ambiguous multi-match: [`LocalDup`](../crates/pkg_a/src/m1.rs#L9-L10)
"#,
    );
    ws.write(
        "docs/archive/old_a.rs",
        "use crate::old_b;\npub fn oa() {}\n",
    );
    ws.write(
        "docs/archive/old_b.rs",
        "use crate::old_a;\npub fn ob() {}\n",
    );

    ws.build();
    let conn = ws.open();
    let opts = page(100, 0, None);
    let res = run_lint(
        &conn,
        &LintRequest {
            target: None,
            paths: None,
            categories: Some(&[
                LintCategory::Docs,
                LintCategory::Syntax,
                LintCategory::Duplicates,
                LintCategory::DeadTails,
                LintCategory::Cycles,
            ]),
            rule_ids: None,
            minimum_severity: Some(LintSeverity::Warn),
            architecture: None,
            workspace_root: Some(ws.root()),
            page: &opts,
        },
    )
    .unwrap();
    let items = res["items"].as_array().unwrap();

    assert!(
        !items.iter().any(|v| v["rule_id"] == "docs.broken_doc_link"),
        "docs.broken_doc_link must be retired from run_lint: {items:?}"
    );
    assert!(
        items.iter().any(|v| v["rule_id"] == "docs.stale_symbol_range" && v["symbol"] == "SharedCfg"),
        "out-of-range #L1-L99 must emit docs.stale_symbol_range in lint mode: {items:?}"
    );
    assert!(
        items.iter().any(|v| v["rule_id"] == "docs.stale_symbol_range" && v["symbol"] == "LocalDup"),
        "multi-match ambiguous symbol claim must emit docs.stale_symbol_range in lint mode: {items:?}"
    );

    assert!(
        items.iter().any(|v| {
            v["rule_id"] == "syntax.intra_crate_type_homonym"
                && v["symbol"]
                    .as_str()
                    .and_then(|s| s.rsplit('.').next())
                    == Some("LocalDup")
                && v["severity"] == "error"
        }),
        "intra-package LocalDup must emit syntax.intra_crate_type_homonym at complexity.severity=error: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["rule_id"] == "syntax.intra_crate_type_homonym"
                && v["symbol"]
                    .as_str()
                    .and_then(|s| s.rsplit('.').next())
                    == Some("SharedCfg")
        }),
        "cross-package SharedCfg (pkg_a vs pkg_b Cargo.toml) must NOT emit intra_crate_type_homonym: {items:?}"
    );

    assert!(
        items.iter().any(|v| v["category"] == "duplicates" && !v["path"].as_str().unwrap_or("").contains("test")),
        "Type-2 renamed-local cross-name functions compute_alpha/compute_beta must emit duplicate clone: {items:?}"
    );
    assert!(
        items.iter().any(|v| v["rule_id"] == "duplicates.similar"),
        "LCS/Jaccard near-clone pair near_alpha/near_beta must emit duplicates.similar: {items:?}"
    );
    assert!(
        !items.iter().any(|v| {
            v["category"] == "duplicates"
                && v["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("test_clone_"))
        }),
        "duplicates must exclude #[cfg(test)] functions by default: {items:?}"
    );

    assert!(
        items.iter().filter(|v| v["category"] == "cycles").count() == 0,
        "test-only and archive import cycles must be filtered from cycles.scc: {items:?}"
    );

    for live_fn in [
        "on_event",
        "custom_hook",
        "comb_cb",
        "depth",
        "schemars_cb",
        "tokens",
        "macro_guard_fn",
        "reexport_fn",
        "build",
        "with_step",
        "finish_step",
    ] {
        assert!(
            !items.iter().any(|v| {
                v["category"] == "dead_tails"
                    && v["symbol"]
                        .as_str()
                        .is_some_and(|s| s.rsplit('.').next() == Some(live_fn))
            }),
            "indexed callback/unresolved receiver '{live_fn}' must remain live: {items:?}"
        );
    }
}
