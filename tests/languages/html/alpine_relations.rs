use super::*;

#[test]
fn alpine_magic_members_require_declared_alpine_dependency() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    let script = "<script>const refs = this.$refs; this.$dispatch('shown');</script>\n";
    workspace.write(
        "declared/package.json",
        r#"{"dependencies":{"alpinejs":"^3.14.0"}}"#,
    );
    workspace.write("declared/index.html", script);
    workspace.write("plain/package.json", r#"{"dependencies":{"vue":"^3.5.0"}}"#);
    workspace.write("plain/index.html", script);

    let conn = workspace.build_and_open();
    let observations = [
        ("declared/index.html", "this.$refs", "external"),
        ("declared/index.html", "this.$dispatch", "external"),
        ("plain/index.html", "this.$refs", "unresolved"),
        ("plain/index.html", "this.$dispatch", "unresolved"),
    ]
    .map(|(path, expression, expected)| {
        let statuses: Vec<(String, String)> = conn
            .prepare("SELECT rc.status,ev.evidence FROM resolution_coverage rc JOIN path_dictionary pd ON pd.path_id=rc.path_id JOIN coverage_expressions ce ON ce.expression_id=rc.expression_id JOIN coverage_evidence ev ON ev.evidence_id=rc.evidence_id WHERE pd.path=?1 AND ce.expression=?2 ORDER BY rc.status,ev.evidence")
            .unwrap()
            .query_map(rusqlite::params![path, expression], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        ((path, expression), (expected, statuses))
    });
    assert!(
        observations
            .iter()
            .all(|(_, (expected, statuses))| {
                statuses.len() == 1 && statuses[0].0.as_str() == *expected
            }),
        "Alpine magic must be external only in the package that declares alpinejs: {observations:#?}"
    );
    commitments::verify(&conn).unwrap();
}

#[test]
fn alpine_syntax_activation_and_x_data_members_without_package_json() {
    let workspace = crate::languages::PersistedWorkspaceHarness::new();
    let alpine_html = concat!(
        "<div x-data=\"{ open: false, toggle() { this.open = !this.open; this.$dispatch('toggle'); }, focusBox() { const refs = this.$refs; return refs; } }\">\n",
        "  <button x-on:click=\"toggle()\" x-bind:class=\"open\">Toggle</button>\n",
        "</div>\n",
    );
    let plain_html = "<div data-state=\"idle\"><script>this.$refs; this.$dispatch('noop');</script></div>\n";
    workspace.write("cdn/page.html", alpine_html);
    workspace.write("cdn/template.j2", "<section x-data=\"{ ready: true, ping() { const el = this.$el; return el; } }\" x-on:click=\"ping()\"></section>\n");
    workspace.write("prefix/page.html", "<button x-on:click=\"this.$dispatch('clicked')\">Go</button>\n");
    workspace.write("eof/unquoted.html", "<div x-data={open:false}><script>this.$dispatch('eof');</script></div>");
    workspace.write("plain/page.html", plain_html);

    let conn = workspace.build_and_open();
    let status_for = |conn: &rusqlite::Connection, path: &str, expression: &str| -> Vec<String> {
        conn.prepare(
            "SELECT DISTINCT rc.status FROM resolution_coverage rc JOIN path_dictionary pd ON pd.path_id=rc.path_id JOIN coverage_expressions ce ON ce.expression_id=rc.expression_id WHERE pd.path=?1 AND ce.expression=?2 ORDER BY rc.status",
        )
        .unwrap()
        .query_map(rusqlite::params![path, expression], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };

    for (path, expression, expected) in [
        ("cdn/page.html", "this.open", vec!["resolved"]),
        ("cdn/page.html", "this.$dispatch", vec!["external"]),
        ("cdn/page.html", "this.$refs", vec!["external"]),
        ("cdn/template.j2", "this.$el", vec!["external"]),
        ("prefix/page.html", "this.$dispatch", vec!["external"]),
        ("eof/unquoted.html", "this.$dispatch", vec!["external"]),
        ("plain/page.html", "this.$refs", vec!["unresolved"]),
        ("plain/page.html", "this.$dispatch", vec!["unresolved"]),
    ] {
        let observed = status_for(&conn, path, expression);
        assert_eq!(
            observed,
            expected,
            "{path} `{expression}` status mismatch"
        );
    }
    let j2_lines: Vec<(i64, i64)> = conn
        .prepare("SELECT n.line, n.end_line FROM nodes n JOIN path_dictionary pd ON pd.path_id=n.path_id WHERE pd.path='cdn/template.j2' AND n.kind='template_scope'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        !j2_lines.is_empty() && j2_lines.iter().all(|&(line, end_line)| line == 1 && end_line == 1),
        "1-line cdn/template.j2 scopes must have line==1 && end_line==1: {j2_lines:?}"
    );
    let eof_errors: i64 = conn
        .query_row(
            "SELECT count(*) FROM errors WHERE path='eof/unquoted.html'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(eof_errors, 0, "unquoted EOF x-data must parse cleanly");
    commitments::verify(&conn).unwrap();
    drop(conn);

    // Removing Alpine syntax attributes deactivates the pack in delta indexing.
    workspace.write("cdn/page.html", plain_html);
    workspace.delta(&["cdn/page.html"]);
    let delta_conn = workspace.open();
    assert_eq!(
        status_for(&delta_conn, "cdn/page.html", "this.$dispatch"),
        vec!["unresolved"]
    );
    assert_eq!(
        status_for(&delta_conn, "cdn/page.html", "this.$refs"),
        vec!["unresolved"]
    );
    commitments::verify(&delta_conn).unwrap();
}
