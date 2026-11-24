use super::*;
#[test]
fn static_module_export_targets() {
    let source = r#"
__all__ = ['ItemA', 'ItemB']
def __getattr__(name):
    if name in __all__:
        import importlib
        return importlib.import_module(f".{name}", __name__)
    raise AttributeError(name)
"#;
    let module_import = source
        .replace("def __getattr__", "import importlib\ndef __getattr__")
        .replace("        import importlib\n", "");
    for (source, supported) in [(source.to_owned(), false), (module_import, true)] {
        let facts = ExtractorHarness::for_lang("python")
            .at_path("package/__init__.py")
            .extract(&source);
        assert!(facts.errors.is_empty(), "{:?}", facts.errors);
        let init = facts.nodes.iter().find(|n| n.kind == acdd_index::models::tags::NodeKind::Module).unwrap();
        if supported {
            let exports = init
                .details
                .get("lazy_exports")
                .and_then(|v| v.as_array())
                .expect("module import and literal __all__ retain lazy export objects");
            let names: Vec<_> = exports
                .iter()
                .map(|v| v["name"].as_str().unwrap())
                .collect();
            assert!(names.contains(&"ItemA"));
            assert!(names.contains(&"ItemB"));
        } else {
            assert!(
                init.details.get("lazy_exports").is_none(),
                "local import is not module authority"
            );
        }
    }

    let (primary, second) = linked_python_roots();
    for w in [&primary, &second] {
        w.write(
            "src/pkg/__init__.py",

            "from . import sibling\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return getattr(sibling, name)\n    raise AttributeError(name)\n",

        );
        w.write("src/pkg/sibling.py", "class Item: pass\n");
        w.write(
            "src/pkg/facade.py",

            "from . import sibling\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return getattr(sibling, name)\n    raise AttributeError(name)\n",

        );
        w.write("src/pkg/direct.py",
 "from .sibling import Item\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return Item\n    raise AttributeError(name)\n");
        w.write(
            "src/pkg/rebound.py",

            "from . import sibling\nsibling = None\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return getattr(sibling, name)\n    raise AttributeError(name)\n",

        );
        w.write(
            "src/pkg/missing.py",

            "from . import unavailable\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return getattr(unavailable, name)\n    raise AttributeError(name)\n",

        );
        w.write(
            "src/pkg/dynamic.py",

            "from . import sibling\n__all__ = ['Item']\ndef __getattr__(name):\n    if name in __all__:\n        return getattr(sibling, name.lower())\n    raise AttributeError(name)\n",

        );
        w.write("src/pkg/namespace/child.py", "def operation(): return 1\n");
        w.write(
            "src/pkg/namespace/child.pyi",
            "def operation() -> int: ...\n",
        );
        w.write(
            "src/pkg/importlib_facade/lazy_module.py",
            "class Child: pass\n",
        );
        w.write("src/pkg/importlib_facade/name.py", "class Nested: pass\n");
        w.write("src/pkg/name.py", "class WrongBase: pass\n");
        w.write("src/pkg/importlib_facade/__init__.py",
 "import importlib\n__all__ = ['lazy_module']\ndef __getattr__(name):\n    if name in __all__:\n        return importlib.import_module(f'.{name}', __name__)\n    raise AttributeError(name)\n");
        w.write("src/pkg/importlib_literal/__init__.py",
 "import importlib\n__all__ = ['name']\ndef __getattr__(export_name):\n    if export_name in __all__:\n        return importlib.import_module('.name', __name__)\n    raise AttributeError(export_name)\n");
        w.write(
            "src/pkg/importlib_literal/name.py",
            "class LiteralTarget: pass\n",
        );
        w.write("src/consumer.py",
 "from pkg import Item\nfrom pkg.facade import Item as facade_item\nfrom pkg.direct import Item as direct_item\nfrom pkg import Unknown\nfrom pkg.rebound import Item as rebound_item\nfrom pkg.missing import Item as missing_item\nfrom pkg.namespace import child\nfrom pkg.dynamic import Item as dynamic_item\nfrom pkg.importlib_facade import lazy_module\nfrom pkg.importlib_literal import name\n");
    }

    type ExportPhase<'a> = (
        &'a [(&'a str, &'a str)],
        &'a str,
        &'a str,
        &'a [&'a str],
        Option<&'a [&'a str]>,
        Option<&'a [&'a str]>,
    );
    type ExportCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [ExportPhase<'a>]);
    let cases: &[ExportCase<'_>] = &[
        ("unique_child_module_is_imported_and_its_calls_resolve",
            &[("pkg/__init__.py",
                "# package\n"),
                ("pkg/child.py",
                "def value(): return 1\n"),
                ("consumer.py",
            "from pkg import child\ndef consume(): return child.value()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "child",
                    "resolved",
                    &["pkg.child.value"] as &[&str],
                    None,
                    Some(&["pkg",
                        "pkg.child"] as &[&str]))]),

        ("cyclic_reexports_and_missing_providers_remain_unresolved",
            &[("pkg/__init__.py",
                "from .a import Config\n"),
                ("pkg/a.py",
                "from .b import Config\n"),
                ("pkg/b.py",
                "from .a import Config\n"),
                ("consumer.py",
            "from pkg import Config\ndef consume(): return Config()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "Config",
                    "unresolved",
                    &[] as &[&str],
                    None,
                    None),
                (&[("pkg/a.py",
                    "from .missing import Config\n")] as &[(&str,
                            &str)],
                    "Config",
                    "unresolved",
                    &[] as &[&str],
                    None,
                    None)]),

        ("bare_dotted_import_does_not_export_child_under_package_binding",
            &[("pkg/__init__.py",
                "# package\n"),
                ("pkg/child.py",
                "def value(): return 1\n"),
                ("api.py",
                "import pkg.child\n"),
                ("consumer.py",
            "from api import pkg\ndef consume(): return pkg.value()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "",
                    "",
                    &[] as &[&str],
                    None,
                    None),
                (&[("api.py",
                        "import pkg.child as alias\n"),
                        ("consumer.py",
                    "from api import alias\ndef consume(): return alias.value()\n")] as &[(&str,
                            &str)],
                    "",
                    "",
                    &["pkg.child.value"] as &[&str],
                    None,
                    None)]),

        ("reassigned_module_export_does_not_retain_original_transitive_target",
            &[("provider.py",
                "class Config: pass\n"),
                ("api.py",
                "from provider import Config\n"),
                ("outer.py",
                "from api import Config\n"),
                ("consumer.py",
            "from outer import Config\ndef consume(): return Config()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "",
                    "",
                    &["provider.Config"] as &[&str],
                    None,
                    None),
                (&[("api.py",
                    "from provider import Config\ndef factory(): return None\nConfig = factory()\n")] as &[(&str,
                            &str)],
                    "",
                    "",
                    &[] as &[&str],
                    None,
                    None),
                (&[("api.py",
                    "from provider import Config\n")] as &[(&str,
                            &str)],
                    "",
                    "",
                    &["provider.Config"] as &[&str],
                    None,
                    None)]),

        ("explicit_package_reexport_resolves_import_and_call",
            &[("pkg/__init__.py",
                "from .logging import get_log\n"),
                ("pkg/logging.py",
                "def get_log(): return 1\n"),
                ("other.py",
                "def helper(): return 2\n"),
                ("consumer.py",
            "from pkg import get_log\ndef consume(): return get_log()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "get_log",
                    "resolved",
                    &["pkg.logging.get_log"] as &[&str],
                    Some(&["pkg/__init__.py",
                        "pkg/logging.py"] as &[&str]),
                    None),
                (&[("pkg/__init__.py",
                    "# export removed\n")] as &[(&str,
                            &str)],
                    "get_log",
                    "unresolved",
                    &[] as &[&str],
                    None,
                    None),
                (&[("pkg/__init__.py",
                    "from .logging import get_log\n")] as &[(&str,
                            &str)],
                    "get_log",
                    "resolved",
                    &["pkg.logging.get_log"] as &[&str],
                    None,
                    None),
                (&[("other.py",
                    "def get_log(): return 2\n")] as &[(&str,
                            &str)],
                    "get_log",
                    "resolved",
                    &["pkg.logging.get_log"] as &[&str],
                    None,
                    None)]),

        ("duplicate_package_reexports_do_not_choose_a_target",
            &[("pkg/__init__.py",
                "from .a import get_log\nfrom .b import get_log\n"),
                ("pkg/a.py",
                "def get_log(): return 1\n"),
                ("pkg/b.py",
                "def get_log(): return 2\n"),
                ("consumer.py",
            "from pkg import get_log\ndef consume(): return get_log()\n")] as &[(&str,
                    &str)],
            &[(&[] as &[(&str,
                            &str)],
                    "get_log",
                    "unresolved",
                    &[] as &[&str],
                    None,
                    None)])
    ];
    // Rewrite complete identifiers in every source/path/expectation together.
    let names = [
        "pkg", "api", "outer", "provider", "other", "child", "consumer",
    ];
    let rewrite = |index: usize, text: &str| {
        let mut result = String::new();
        let mut word = String::new();
        let flush = |word: &mut String, result: &mut String| {
            if names.contains(&word.as_str()) {
                result.push_str(&format!("case_{index}_"));
            }
            result.push_str(word);
            word.clear();
        };
        for ch in text.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                word.push(ch);
            } else {
                flush(&mut word, &mut result);
                result.push(ch);
            }
        }
        flush(&mut word, &mut result);
        result
    };
    for (index, (_, files, _)) in cases.iter().enumerate() {
        for &(path, source) in *files {
            primary.write(
                format!("src/{}", rewrite(index, path)),
                &rewrite(index, source),
            );
        }
    }
    primary.build();
    let conn = primary.open();

    for (consumer, prefix) in [
        ("src/consumer.py", "src/"),
        ("[second]/src/consumer.py", "[second]/src/"),
    ] {
        for (line, name) in [(1, "Item"), (2, "Item"), (3, "Item")] {
            let (status, targets) = persisted_import(&conn, consumer, line, name);
            assert_eq!(status, "resolved", "{consumer}:{line}: {targets:?}");
            assert!(
                targets.contains(&format!("{prefix}pkg/sibling.py:Item")),
                "{consumer}:{line}: {targets:?}"
            );
            assert!(
                !targets
                    .iter()
                    .any(|target| target.starts_with(if prefix == "src/" {
                        "[second]/"
                    } else {
                        "src/"
                    })),
                "cross-project target: {targets:?}"
            );
        }
        let (status, targets) = persisted_import(&conn, consumer, 7, "child");
        assert_eq!(
            status, "resolved",
            "{consumer} namespace child: {targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|target| target.starts_with(&format!("{prefix}pkg/namespace/child.py:"))),
            "{targets:?}"
        );
        assert!(
            !targets
                .iter()
                .any(|target| target.starts_with(&format!("{prefix}pkg/namespace/child.pyi:"))),
            "stub was selected over runtime: {targets:?}"
        );
        let (status, targets) = persisted_import(&conn, consumer, 9, "lazy_module");
        assert_eq!(
            status, "resolved",
            "{consumer} importlib module: {targets:?}"
        );
        assert!(
            targets.iter().any(|target| target
                .starts_with(&format!("{prefix}pkg/importlib_facade/lazy_module.py:"))),
            "{targets:?}"
        );
        let (status, targets) = persisted_import(&conn, consumer, 10, "name");
        assert_eq!(
            status, "resolved",
            "{consumer} literal importlib module: {targets:?}"
        );
        assert!(
            targets.iter().any(
                |target| target.starts_with(&format!("{prefix}pkg/importlib_literal/name.py:"))
            ),
            "{targets:?}"
        );
        assert!(
            !targets
                .iter()
                .any(|target| target.starts_with(&format!("{prefix}pkg/name.py:"))),
            "wrong importlib base: {targets:?}"
        );
        for (line, name) in [(4, "Unknown"), (5, "Item"), (6, "Item"), (8, "Item")] {
            let (status, targets) = persisted_import(&conn, consumer, line, name);
            assert_eq!(status, "unresolved", "{consumer}:{line}: {targets:?}");
            assert!(
                !targets
                    .iter()
                    .any(|target| target.ends_with("sibling.py:Item")),
                "{consumer}:{line}: {targets:?}"
            );
        }
    }

    drop(conn);
    for (index, &(case_id, initial_files, phases)) in cases.iter().enumerate() {
        let path = format!("src/{}", rewrite(index, "consumer.py"));
        for (changes, expression, expected_status, calls, import_paths, import_names) in phases {
            let changed: Vec<_> = changes
                .iter()
                .map(|(path, source)| {
                    let path = format!("src/{}", rewrite(index, path));
                    primary.write(&path, &rewrite(index, source));
                    path
                })
                .collect();
            if !changed.is_empty() {
                primary.delta(&changed.iter().map(String::as_str).collect::<Vec<_>>());
                let parity = crate::languages::harness::cold_relations(&primary);
                assert_eq!(parity.incremental_verification, Ok(()), "{case_id}");
                assert_eq!(parity.cold_verification, Ok(()), "{case_id}");
                assert_eq!(parity.incremental, parity.cold, "{case_id}");
            }
            let conn = primary.open();
            if !expression.is_empty() {
                assert_eq!(
                    persisted_coverage(&conn, &path, 1, &rewrite(index, expression)).0,
                    *expected_status,
                    "{case_id}"
                );
            }
            for (kind, field, expected) in [
                ("calls", "dst.qualname", Some(*calls)),
                ("imports", "p.path", *import_paths),
                ("imports", "dst.qualname", *import_names),
            ] {
                if let Some(expected) = expected {
                    let sql = format!("SELECT {field} FROM edges e JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash JOIN path_dictionary p ON p.path_id=dst.path_id JOIN path_dictionary owner ON owner.path_id=src.path_id WHERE owner.path=?1 AND e.kind=?2 ORDER BY {field}");
                    let actual = query_strings(&conn, &sql, (&path, kind));
                    let expected: Vec<_> = expected
                        .iter()
                        .map(|s| {
                            if field == "p.path" {
                                format!("src/{}", rewrite(index, s))
                            } else {
                                format!("src.{}", rewrite(index, s))
                            }
                        })
                        .collect();
                    assert_eq!(actual, expected, "{case_id}: {kind} {field}");
                }
            }
        }
        let restored: Vec<_> = initial_files
            .iter()
            .map(|(path, source)| {
                let path = format!("src/{}", rewrite(index, path));
                primary.write(&path, &rewrite(index, source));
                path
            })
            .collect();
        primary.delta(&restored.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

#[test]
fn ordinary_module_and_transitive_reexports_preserve_delta_resolution() {
    let w = fixture_workspace(&[
        ("pkg/__init__.py", "from .api import Config\n"),
        ("pkg/api.py", "from .models import Config\n"),
        ("pkg/models.py", "class Config: pass\n"),
        ("pkg/alternate.py", "class Config: pass\n"),
        (
            "consumer.py",
            "from pkg import Config\ndef consume(): return Config()\n",
        ),
    ]);
    w.build();
    assert_eq!(status(&w.open(), "Config"), "resolved");
    assert_eq!(call_target_paths(&w.open()), ["pkg/models.py"]);
    w.write("pkg/api.py", "from .alternate import Config\n");
    w.delta(&["pkg/api.py"]);
    assert_eq!(call_target_paths(&w.open()), ["pkg/alternate.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
    w.write(
        "consumer.py",
        "import pkg.api as api\ndef consume(): return api.Config()\n",
    );
    w.delta(&["consumer.py"]);
    assert_eq!(call_target_paths(&w.open()), ["pkg/alternate.py"]);
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
}

#[test]
fn lazy_export_provider_deltas() {
    let w = fixture_workspace(&[("pkg/__init__.py",
"# package\n"),
 ("pkg/models.py",
"class Config: pass\n"),
 ("pkg/api.py",
"_EXPORTS = frozenset({'Config'})\ndef __getattr__(name):\n    if name in _EXPORTS:\n        from . import models\n        return getattr(models, name)\n    raise AttributeError(name)\n"),
 ("consumer.py",
"from pkg.api import Config\ndef consume(): return Config()\n"),
 ("relative_pkg/__init__.py",
"# package\n"),
 ("relative_pkg/relative_child.py",
"class Config: pass\n"),
 ("relative_child.py",
"class Config: pass\n"),
 ("relative_pkg/service.py",
"import importlib\ndef __getattr__(name):\n    if name == 'relative_child':\n        return importlib.import_module('..relative_child', __name__)\n    raise AttributeError(name)\n"),
 ("relative_consumer.py",
"from relative_pkg.service import relative_child\ndef consume(): return relative_child.Config()\n")] as &[(&str,
&str)]);
    w.build();
    for (consumer, expression, expected) in [
        ("consumer.py", "Config", "pkg/models.py"),
        (
            "relative_consumer.py",
            "relative_child",
            "relative_pkg/relative_child.py",
        ),
    ] {
        let conn = w.open();
        assert_eq!(
            persisted_coverage(&conn, consumer, 1, expression).0,
            "resolved"
        );
        let paths = query_strings(&conn,
 "SELECT p.path FROM edges e JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash JOIN path_dictionary p ON p.path_id=dst.path_id JOIN path_dictionary owner ON owner.path_id=src.path_id WHERE owner.path=?1 AND e.kind='calls' ORDER BY p.path",
 [consumer]);
        assert_eq!(paths, [expected]);
    }
    for (case_id,
 consumer,
 expression,
 path,
 source,
 status,
 expected) in [
("literal_lazy_exports_resolve_existing_providers_and_track_delta",
"consumer.py",
"Config",
"pkg/api.py",
"_EXPORTS = frozenset({'Config'})\ndef __getattr__(name):\n    if name in _EXPORTS:\n        from . import models\n        return getattr(models, name)\n    raise AttributeError(name)\n",
"resolved",
&["pkg/models.py"] as &[&str]),

("literal_lazy_exports_resolve_existing_providers_and_track_delta",
"consumer.py",
"Config",
"pkg/api.py",
"_EXPORTS = frozenset({'Config'})\ndef __getattr__(name):\n    if name in _EXPORTS:\n        from . import missing\n        return getattr(missing, name)\n    raise AttributeError(name)\n",
"unresolved",
&[] as &[&str]),

("lazy_importlib_module_relative_namespace_is_preserved_in_delta",
"relative_consumer.py",
"relative_child",
"relative_consumer.py",
"from relative_pkg.service import relative_child\ndef consume():\n    return relative_child.Config()\n",
"resolved",
&["relative_pkg/relative_child.py"] as &[&str]),

("lazy_importlib_module_relative_namespace_is_preserved_in_delta",
"relative_consumer.py",
"relative_child",
"relative_pkg/service.py",
"import importlib\ndef __getattr__(name):\n    if name == 'relative_child':\n        return importlib.import_module('.relative_child', __name__)\n    raise AttributeError(name)\n",
"unresolved",
&[] as &[&str])
    ] {
        w.write(path,
 source);
        w.delta(&[path]);
        let conn = w.open();
        assert_eq!(persisted_coverage(&conn,
 consumer,
 1,
 expression).0,
 status,
 "{case_id}");
        let paths = query_strings(&conn,
 "SELECT p.path FROM edges e JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash JOIN path_dictionary p ON p.path_id=dst.path_id JOIN path_dictionary owner ON owner.path_id=src.path_id WHERE owner.path=?1 AND e.kind='calls' ORDER BY p.path",
 [consumer]);
        assert_eq!(paths,
 expected,
 "{case_id}");
        let parity = crate::languages::harness::cold_relations(&w);
        assert_eq!(parity.incremental_verification,
 Ok(()));
        assert_eq!(parity.cold_verification,
 Ok(()));
        assert_eq!(parity.incremental,
 parity.cold);
    }
}

#[test]
fn src_layout_linked_reexports_preserve_owner_workspace_priority() {
    let w = fixture_workspace(&[]);
    let linked = fixture_workspace(&[
        ("src/catalogue/__init__.py", "from .api import Config\n"),
        ("src/catalogue/api.py", "from .models import Config\n"),
        ("src/catalogue/models.py", "class Config: pass\n"),
    ]);
    w.write(
        "acdd.yaml",
        &format!(
            "roots: [src]\nlinked_workspaces:\n  - name: catalogue\n    path: '{}'\n    roots: [src]\n",
            linked.root().display()
        ),
    );
    w.write(
        "src/consumer.py",
        "from catalogue.api import Config\ndef consume(): return Config()\n",
    );
    w.build();
    let targets = |conn: &Connection| {
        query_strings(
            conn,
            "SELECT (SELECT path FROM path_dictionary WHERE path_id=dst.path_id) FROM edges e JOIN nodes src ON src.node_hash=e.src_hash JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE src.path_id=(SELECT path_id FROM path_dictionary WHERE path='src/consumer.py') AND e.kind='calls' ORDER BY (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)",
            [],
        )
    };
    assert_eq!(targets(&w.open()), ["[catalogue]/src/catalogue/models.py"]);
    w.write("src/catalogue/__init__.py", "from .api import Config\n");
    w.write("src/catalogue/api.py", "from .models import Config\n");
    w.write("src/catalogue/models.py", "class Config: pass\n");
    w.build();
    assert_eq!(targets(&w.open()), ["src/catalogue/models.py"]);
    w.write("src/catalogue/api.py", "from .missing import Config\n");
    w.delta(&["src/catalogue/api.py"]);
    assert!(targets(&w.open()).is_empty());
    let parity = crate::languages::harness::cold_relations(&w);
    assert_eq!(parity.incremental_verification, Ok(()));
    assert_eq!(parity.cold_verification, Ok(()));
    assert_eq!(parity.incremental, parity.cold);
}
