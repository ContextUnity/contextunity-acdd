use super::*;

#[test]
fn context_only_html_deltas() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    // Preserve the two original activation configurations in isolated projects.
    let cases = [
        (
            "unrelated_html_delta_does_not_hydrate_jinja_loader_providers",
            "loader", "['jinja2>=3']", "loader/environment.py",
            "from pathlib import Path\nfrom jinja2 import Environment, FileSystemLoader\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n",
            "loader/unrelated.html", Some(0), false,
        ),
        (
            "html_delta_does_not_reinsert_render_dependencies_for_context_only_loader_facts",
            "render", "['django>=5', 'jinja2>=3']", "render/views.py",
            "from pathlib import Path\nfrom django.shortcuts import render\nfrom jinja2 import Environment, FileSystemLoader\nTEMPLATES = Path(__file__).parent / 'templates'\nenv = Environment(loader=FileSystemLoader(str(TEMPLATES)))\n\ndef page(request):\n    return render(request, 'templates/other.html', {'title': 'Other'})\n",
            "render/templates/page.html", None, true,
        ),
    ];
    for (_, project, dependencies, provider, source, changed, _, _) in cases {
        workspace.write(
            format!("{project}/pyproject.toml"),
            &format!("[project]\nname = '{project}'\ndependencies = {dependencies}\n"),
        );
        workspace.write(provider, source);
        workspace.write(
            format!("{project}/templates/page.html"),
            "{% set title = 'Welcome' %}{{ title }}\n",
        );
        workspace.write(format!("{project}/templates/other.html"), "<p>Other</p>\n");
        workspace.write(changed, "<p>Before</p>\n");
    }
    workspace.build();
    let render_dependencies = |conn: &rusqlite::Connection, provider: &str| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM dependencies d JOIN path_dictionary p ON p.path_id=d.owner_id WHERE p.path=?1 AND d.kind='renders'",
            [provider], |row| row.get(0),
        ).unwrap()
    };
    for (case_id, _, _, provider, _, changed, loaded, has_render) in cases {
        let before_count = render_dependencies(&workspace.open(), provider);
        if has_render {
            assert!(
                before_count > 0,
                "{case_id}: fixture must persist a render dependency"
            );
        }
        workspace.write(changed, "<p>After</p>\n");
        let delta = workspace.delta(&[changed]);
        if let Some(expected) = loaded {
            assert_eq!(delta["loaded_fact_files"], expected, "{case_id}: {delta}");
        }
        let conn = workspace.open();
        assert_eq!(
            render_dependencies(&conn, provider),
            before_count,
            "{case_id}"
        );
        commitments::verify(&conn).unwrap();
        drop(conn);
        workspace.write(changed, "<p>Before</p>\n");
        workspace.delta(&[changed]);
    }
}
