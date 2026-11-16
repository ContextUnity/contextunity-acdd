use super::*;

fn linked_provider_workspace() -> (PersistedWorkspaceHarness, PersistedWorkspaceHarness) {
    let owner = crate::languages::PersistedWorkspaceHarness::new();
    let linked = crate::languages::PersistedWorkspaceHarness::new();
    owner.write(
        "acdd.yaml",
        &format!(
            "roots: [.]\nlinked_workspaces:\n  - name: provider\n    path: '{}'\n    roots: [.]\n",
            linked.root().display()
        ),
    );
    (owner, linked)
}

#[cfg(feature = "lang-python")]
#[test]
fn persisted_import_identity() {
    let owner = crate::languages::PersistedWorkspaceHarness::new();
    let a = crate::languages::PersistedWorkspaceHarness::new();
    let b = crate::languages::PersistedWorkspaceHarness::new();
    owner.write("acdd.yaml", &format!("roots: [.]\nlinked_workspaces:\n  - name: a\n    path: '{}'\n    roots: [.]\n  - name: b\n    path: '{}'\n    roots: [.]\n",a.root().display(),b.root().display()));
    owner.write("settings.py", "def value(): pass\n");
    owner.write("shared/service.py", "def local(): pass\n");
    owner.write("foreign/only.ts", "export function target() {}\n");
    owner.write("main.py","from settings import value\nfrom shared.service import local\nfrom foreign.only import target\nfrom dup.provider import chosen\ndef use():\n    value()\n    local()\n    target()\n    chosen()\n");
    a.write("settings.py", "def value(): pass\n");
    a.write("shared/service.py", "def local(): pass\n");
    a.write("pkg/helpers.py", "def helper(): pass\n");
    a.write("pkg/use.py","from .helpers import helper\nfrom ...settings import value\ndef use():\n    helper()\n    value()\n");
    a.write("dup/provider.py", "def chosen(): pass\n");
    b.write("dup/provider.py", "def other(): pass\n");

    owner.write("unverified.py", "from unmatched_settings import value\nfrom unmatched_path import Path\nimport time\nfrom typing import Any\nfrom commerce.snapshot import revert\ndef use():\n    value()\n    Path()\n    time.time()\n    Any()\n    revert()\n");
    a.write("package/unmatched_settings.py", "def value(): pass\n");
    a.write("package/unmatched_path.py", "class Path: pass\n");
    a.write("package/time.py", "def time(): pass\n");
    a.write("package/typing.py", "class Any: pass\n");
    a.write("commerce/snapshot.py", "def revert(): pass\n");
    owner.build();
    let conn = owner.open();

    let mut st = conn.prepare("SELECT (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash) FROM edge_occurrences WHERE (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)='unverified.py' AND kind IN ('imports','calls') ORDER BY (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash)").unwrap();
    let targets: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        targets.len(),
        3,
        "the qualified module import, symbol import, and call must resolve: {targets:?}"
    );
    assert!(targets.iter().all(|t| t.contains("commerce/snapshot.py")));
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.starts_with("module:"))
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.ends_with(":revert"))
            .count(),
        2
    );
    let unresolved: i64 = count(&conn, "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='unverified.py' AND status='unresolved'");
    assert_eq!(
        unresolved, 0,
        "unmatched absolute imports and calls are retained as unverified external dependencies"
    );
    let external: i64 = count(&conn, "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='unverified.py' AND status='external'");
    assert_eq!(
        external, 8,
        "four unmatched absolute imports and their calls are classified as external"
    );
    let unverified_external: i64 = count(&conn, "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='unverified.py' AND status='external' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id) IN ('value','Path') AND (SELECT evidence FROM coverage_evidence WHERE evidence_id=resolution_coverage.evidence_id) LIKE '%no indexed provider%'");
    assert_eq!(unverified_external, 2);
    #[cfg(feature = "lang-typescript")]
    {
        let mut st = conn
        .prepare("SELECT (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id), (SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash) FROM edge_occurrences WHERE kind='calls' AND (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id)!='unverified.py' ORDER BY (SELECT path FROM path_dictionary WHERE path_id=edge_occurrences.owner_id),(SELECT id FROM nodes WHERE node_hash=edge_occurrences.dst_hash)")
        .unwrap();
        let calls: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls.contains(&("main.py".into(), "py:settings.py:1:value".into())));
        assert!(calls.contains(&("main.py".into(), "py:shared/service.py:1:local".into())));
        assert!(calls.contains(&(
            "[a]/pkg/use.py".into(),
            "py:[a]/pkg/helpers.py:1:helper".into()
        )));
        let ambiguous:i64=count(&conn, "SELECT count(*) FROM resolution_coverage WHERE (SELECT path FROM path_dictionary WHERE path_id=resolution_coverage.path_id)='main.py' AND (SELECT expression FROM coverage_expressions WHERE expression_id=resolution_coverage.expression_id)='chosen' AND status='ambiguous'");
        assert_eq!(ambiguous, 1);
    }
}

#[cfg(feature = "lang-python")]
#[test]
fn import_provider_changes_delta_matches_cold_build() {
    let (owner, linked) = linked_provider_workspace();
    owner.write(
        "main.py",
        "from pkg.service import run\ndef use(): return run()\n",
    );
    linked.write("pkg/service.py", "def run(): pass\n");
    owner.build();
    for step in 0..5 {
        let changed = match step {
            0 => {
                linked.write("pkg/service.py", "\ndef run(): pass\n");
                vec!["[provider]/pkg/service.py"]
            }
            1 => {
                fs::rename(
                    linked.root().join("pkg/service.py"),
                    linked.root().join("pkg/renamed.py"),
                )
                .unwrap();
                vec!["[provider]/pkg/service.py", "[provider]/pkg/renamed.py"]
            }
            2 => {
                linked.write("pkg/service.py", "def run(): pass\n");
                vec!["[provider]/pkg/service.py"]
            }
            3 => {
                linked.write("pkg/service.pyi", "def unrelated(): pass\n");
                vec!["[provider]/pkg/service.pyi"]
            }
            _ => {
                linked.remove("pkg/service.pyi");
                vec!["[provider]/pkg/service.pyi"]
            }
        };
        owner.delta(&changed);
        let delta = persisted_graph(&owner);
        owner.build();
        let cold = persisted_graph(&owner);
        assert_eq!(delta, cold, "provider change step {step}");
    }
}
