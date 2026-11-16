use super::*;

#[test]
fn jinja_loader_provenance() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    workspace.write(
        "shop/pyproject.toml",
        "[project]\nname = 'shop'\ndependencies = ['jinja2>=3']\n",
    );
    workspace.write(
        "shop/site/environment.py",
        "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n",
    );
    let source = "{% set rows = [] %}\n{{ rows|selectattr('active')|list }}\n";
    let rooted_source = format!("{source}{{% include 'partials/card.html' %}}\n");
    workspace.write("shop/site/templates/page.html", &rooted_source);
    workspace.write("shop/site/other/templates/page.html", source);
    workspace.write(
        "shop/site/templates/partials/card.html",
        "<article>{{ row.name }}</article>\n",
    );

    let source = "{% set rows = [] %}\n{{ rows|selectattr('active')|list }}\n";
    for project in [
        "ambiguous",
        "rebound",
        "annotated",
        "tuple",
        "local_shadow",
        "src_jinja_shadow",
        "src_path_shadow",
        "missing",
    ] {
        workspace.write(
            format!("{project}/pyproject.toml"),
            &format!("[project]\nname = '{project}'\ndependencies = ['jinja2>=3']\n"),
        );
        workspace.write(format!("{project}/templates/page.html"), source);
    }
    let provider = "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n";
    workspace.write("ambiguous/first.py", provider);
    workspace.write("ambiguous/second.py", provider);
    workspace.write("rebound/environment.py", "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nFileSystemLoader = object()\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n");
    workspace.write("annotated/environment.py", "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nFileSystemLoader: object = object()\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n");
    workspace.write("tuple/environment.py", "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nFileSystemLoader, unused = (object(), None)\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n");
    workspace.write("local_shadow/jinja2.py", "def Environment(*args, **kwargs):\n    return None\n\ndef FileSystemLoader(*args, **kwargs):\n    return None\n");
    workspace.write("local_shadow/environment.py", provider);
    workspace.write("src_jinja_shadow/src/jinja2.py", "def Environment(*args, **kwargs):\n    return None\n\ndef FileSystemLoader(*args, **kwargs):\n    return None\n");
    workspace.write("src_jinja_shadow/environment.py", provider);
    workspace.write("src_path_shadow/src/pathlib.py", "class Path:\n    pass\n");
    workspace.write("src_path_shadow/environment.py", provider);

    let template = "{% set rows = [] %}\n{{ rows|e }}\n";
    for project in [
        "proven",
        "dynamic_parent",
        "bare_file_parent",
        "string_resolve",
        "rebound_loader",
        "shadowed_str",
        "shadowed_path",
        "shadowed_root",
        "conditional_return",
    ] {
        workspace.write(
            format!("{project}/pyproject.toml"),
            &format!("[project]\nname = '{project}'\ndependencies = ['jinja2>=3']\n"),
        );
        workspace.write(
            format!("{project}/src/pkg/templates/spec/page.html"),
            template,
        );
    }
    workspace.write("proven/src/pkg/templates/other/page.html", template);
    let provider = "from functools import lru_cache\nfrom pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\n_PACKAGE_ROOT = Path(__file__).resolve().parents[1]\n_TEMPLATE_DIR = _PACKAGE_ROOT / 'templates' / 'spec'\n\n@lru_cache(maxsize=1)\ndef environment():\n    loader = FileSystemLoader(str(_TEMPLATE_DIR))\n    return Environment(loader=loader)\n";
    workspace.write("proven/src/pkg/render/jinja.py", provider);
    workspace.write(
        "dynamic_parent/src/pkg/render/jinja.py",
        &provider
            .replace("parents[1]", "parents[INDEX]")
            .replace("_PACKAGE_ROOT =", "INDEX = 1\n_PACKAGE_ROOT ="),
    );
    workspace.write(
        "bare_file_parent/src/pkg/render/jinja.py",
        &provider.replace(
            "Path(__file__).resolve().parents[1]",
            "__file__.resolve().parents[1]",
        ),
    );
    workspace.write(
        "string_resolve/src/pkg/render/jinja.py",
        &provider.replace(
            "Path(__file__).resolve().parents[1]",
            "str(Path(__file__)).resolve().parents[1]",
        ),
    );
    workspace.write(
        "rebound_loader/src/pkg/render/jinja.py",
        &provider.replace(
            "    return Environment(loader=loader)",
            "    loader = unknown\n    return Environment(loader=loader)",
        ),
    );
    workspace.write(
        "shadowed_str/src/pkg/render/jinja.py",
        &provider.replace(
            "    loader = FileSystemLoader",
            "    str = lambda value: unknown\n    loader = FileSystemLoader",
        ),
    );
    workspace.write("shadowed_path/src/pkg/render/jinja.py", &provider.replace("    loader = FileSystemLoader(str(_TEMPLATE_DIR))", "    Path = lambda value: unknown\n    loader = FileSystemLoader(str(Path(__file__).resolve().parents[1] / 'templates' / 'spec'))"));
    workspace.write(
        "shadowed_root/src/pkg/render/jinja.py",
        &provider.replace(
            "    loader = FileSystemLoader",
            "    _TEMPLATE_DIR = unknown\n    loader = FileSystemLoader",
        ),
    );
    workspace.write(
        "conditional_return/src/pkg/render/jinja.py",
        &provider.replace(
            "    return Environment(loader=loader)",
            "    if False:\n        return Environment(loader=loader)",
        ),
    );

    workspace.build();
    let status = |conn: &rusqlite::Connection, path: &str, expression: &str| -> String {
        conn.query_row(
            "SELECT rc.status FROM resolution_coverage rc JOIN path_dictionary pd ON pd.path_id=rc.path_id JOIN coverage_expressions ce ON ce.expression_id=rc.expression_id WHERE pd.path=?1 AND ce.expression=?2",
            [path, expression],
            |row| row.get(0),
        )
        .unwrap()
    };
    let include_targets = |conn: &rusqlite::Connection, path: &str| -> Vec<String> {
        conn.prepare("SELECT (SELECT path FROM path_dictionary WHERE path_id=dst.path_id) FROM edge_occurrences e JOIN nodes dst ON dst.node_hash=e.dst_hash WHERE (SELECT path FROM path_dictionary WHERE path_id=e.owner_id)=?1 AND e.kind='includes' ORDER BY (SELECT path FROM path_dictionary WHERE path_id=dst.path_id)")
            .unwrap()
            .query_map([path], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    let mut cases: Vec<(&str, String, &[&str], &str)> = vec![
        (
            "jinja_builtins_follow_imported_filesystem_loader_roots",
            "shop/site/templates/page.html".into(),
            &[
                "template.tag.set",
                "template.filter.selectattr",
                "template.filter.list",
            ],
            "external",
        ),
        (
            "jinja_builtins_follow_imported_filesystem_loader_roots",
            "shop/site/other/templates/page.html".into(),
            &[
                "template.tag.set",
                "template.filter.selectattr",
                "template.filter.list",
            ],
            "unresolved",
        ),
        (
            "jinja_loader_roots_follow_literal_parent_chains_and_local_loader_flow",
            "proven/src/pkg/templates/spec/page.html".into(),
            &["template.tag.set", "template.filter.e"],
            "external",
        ),
        (
            "jinja_loader_roots_follow_literal_parent_chains_and_local_loader_flow",
            "proven/src/pkg/templates/other/page.html".into(),
            &["template.tag.set", "template.filter.e"],
            "unresolved",
        ),
    ];
    for project in [
        "ambiguous",
        "rebound",
        "annotated",
        "tuple",
        "local_shadow",
        "src_jinja_shadow",
        "src_path_shadow",
        "missing",
    ] {
        cases.push((
            "jinja_loader_roots_fail_closed_without_a_unique_imported_provider",
            format!("{project}/templates/page.html"),
            &[
                "template.tag.set",
                "template.filter.selectattr",
                "template.filter.list",
            ],
            "unresolved",
        ));
    }
    for project in [
        "dynamic_parent",
        "bare_file_parent",
        "string_resolve",
        "rebound_loader",
        "shadowed_str",
        "shadowed_path",
        "shadowed_root",
        "conditional_return",
    ] {
        cases.push((
            "jinja_loader_roots_follow_literal_parent_chains_and_local_loader_flow",
            format!("{project}/src/pkg/templates/spec/page.html"),
            &["template.tag.set", "template.filter.e"],
            "unresolved",
        ));
    }
    let conn = workspace.open();
    for (case_id, path, expressions, expected) in &cases {
        for expression in *expressions {
            assert_eq!(
                status(&conn, path, expression),
                *expected,
                "{case_id}: {path} {expression}"
            );
        }
    }
    let expected_include = ["shop/site/templates/partials/card.html"];
    assert_eq!(
        include_targets(&conn, "shop/site/templates/page.html"),
        expected_include
    );
    drop(conn);

    for (case_id, path, source, retargeted) in [
        ("jinja_builtins_follow_imported_filesystem_loader_roots", "shop/site/templates/page.html", format!("{rooted_source}<p>Changed</p>\n"), false),
        ("jinja_loader_roots_follow_literal_parent_chains_and_local_loader_flow", "proven/src/pkg/templates/spec/page.html", "{% set rows = [] %}\n{{ rows|e }}\n<p>Changed</p>\n".into(), false),
        ("jinja_builtins_follow_imported_filesystem_loader_roots", "shop/site/environment.py", "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nTEMPLATES = Path(__file__).parent / 'other' / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n".into(), true),
    ] {
        workspace.write(path, &source);
        workspace.delta(&[path]);
        let cold_db = workspace.root().join("jinja-cold.sqlite");
        workspace.build_to(&cold_db);
        let delta = workspace.open();
        let cold = workspace.open_at(&cold_db);
        for (original, template, expressions, expected) in &cases {
            let expected = if retargeted && template == "shop/site/templates/page.html" { "unresolved" }
                else if retargeted && template == "shop/site/other/templates/page.html" { "external" }
                else { *expected };
            for expression in *expressions {
                assert_eq!(status(&delta, template, expression), expected, "{case_id}: {original}: {template} {expression}");
                assert_eq!(status(&delta, template, expression), status(&cold, template, expression), "{template} {expression}");
            }
        }
        if !retargeted {
            assert_eq!(include_targets(&delta, "shop/site/templates/page.html"), expected_include);
            assert_eq!(include_targets(&delta, "shop/site/templates/page.html"), include_targets(&cold, "shop/site/templates/page.html"));
        }
        commitments::verify(&delta).unwrap();
        commitments::verify(&cold).unwrap();
    }
}
