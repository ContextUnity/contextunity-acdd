use super::*;
use acdd_index::db::reader::SelectorError;

#[test]
fn scoped_selector_resolves_matching_symbol() {
    let conn = search_and_paging::search_workspace().open();
    let result = reader::inspect_paged_with_path(
        &conn,
        "admit",
        Some("pkg/service.py"),
        false,
        &options(10),
    )
    .unwrap();
    assert_eq!(result["node"]["name"], "admit");
}

#[test]
fn selector_colon_scoped_suffix_and_one_hop_imports() {
    let workspace = Workspace::new();
    workspace.write(
        "src/engine.rs",
        "pub struct Engine;\nimpl Engine {\n    pub fn run(&self) -> usize { 1 }\n}\npub struct OtherEngine;\nimpl OtherEngine {\n    pub fn run(&self) -> usize { 2 }\n}\n",
    );
    workspace.write(
        "pkg/provider.py",
        "def helper():\n    return 42\n\ndef local_wins():\n    return 'provider'\n",
    );
    workspace.write(
        "pkg/barrel.py",
        "from pkg.provider import helper\n\ndef _private_sibling():\n    return 0\n",
    );
    workspace.write("pkg/barrel_consumer.py", "from pkg.barrel import helper\n");
    workspace.write(
        "pkg/consumer.py",
        "from pkg.provider import helper, local_wins\n\ndef local_wins():\n    return 'consumer'\n",
    );
    workspace.write("pkg/dup_a.py", "def clash():\n    return 'a'\n");
    workspace.write("pkg/dup_b.py", "def clash():\n    return 'b'\n");
    workspace.write(
        "pkg/ambig.py",
        "from pkg.dup_a import clash as clash_a\nfrom pkg.dup_b import clash as clash_b\n",
    );
    workspace.build();
    let conn = workspace.open();

    // Table-driven positive resolution matrix: Type::method, scoped suffix, path-qualified, 1-hop import & barrel re-export, local precedence
    for (selector, path, expected_path, expected_name) in [
        ("Engine::run", None, "src/engine.rs", "run"),
        ("Engine::run", Some("src/engine.rs"), "src/engine.rs", "run"),
        ("Engine.run", Some("src/engine.rs"), "src/engine.rs", "run"),
        ("src/engine.rs:Engine::run", None, "src/engine.rs", "run"),
        ("src/engine.rs::Engine::run", None, "src/engine.rs", "run"),
        (
            "helper",
            Some("pkg/consumer.py"),
            "pkg/provider.py",
            "helper",
        ),
        ("helper", Some("pkg/barrel.py"), "pkg/provider.py", "helper"),
        (
            "helper",
            Some("pkg/barrel_consumer.py"),
            "pkg/provider.py",
            "helper",
        ),
        (
            "local_wins",
            Some("pkg/consumer.py"),
            "pkg/consumer.py",
            "local_wins",
        ),
    ] {
        let inspected = reader::inspect_paged_with_path(&conn, selector, path, false, &options(10))
            .unwrap_or_else(|err| panic!("selector={selector:?} path={path:?} failed: {err}"));
        assert_eq!(
            inspected["node"]["path"], expected_path,
            "selector={selector:?} path={path:?}"
        );
        assert_eq!(
            inspected["node"]["name"], expected_name,
            "selector={selector:?} path={path:?}"
        );
    }

    for (selector, path) in [
        ("ngine::run", "src/engine.rs"),
        ("_private_sibling", "pkg/barrel_consumer.py"),
        ("local_wins", "pkg/barrel.py"),
    ] {
        let error =
            reader::inspect_paged_with_path(&conn, selector, Some(path), false, &options(10))
                .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SelectorError>(),
                Some(SelectorError::NotFound { .. })
            ),
            "unexpected result for {selector} in {path}: {error}"
        );
    }

    // 1-hop imported/re-exported ambiguity must fail closed with SelectorError::Ambiguous
    let ambig_err =
        reader::inspect_paged_with_path(&conn, "clash", Some("pkg/ambig.py"), false, &options(10))
            .unwrap_err();
    let Some(SelectorError::Ambiguous { candidates, .. }) =
        ambig_err.downcast_ref::<SelectorError>()
    else {
        panic!("expected SelectorError::Ambiguous for 1-hop clash, got: {ambig_err}");
    };
    let candidate_paths: std::collections::BTreeSet<&str> =
        candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        candidate_paths,
        ["pkg/dup_a.py", "pkg/dup_b.py"].into_iter().collect()
    );
}
