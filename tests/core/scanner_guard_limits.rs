use crate::common::Workspace;
use acdd_index::scanner;
use contextunity_acdd::engine::pipeline as writer;
use rusqlite::Connection;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[test]
fn scanner_scope_and_limits_matrix() {
    let workspace = Workspace::new();
    workspace.write("repo1/.git/config", "");
    workspace.write("repo1/src/lib.py", "def a(): pass");
    workspace.write("repo2/.git/config", "");
    workspace.write("repo2/src/lib.py", "def b(): pass");
    for (root, allow_broad, manifest, expected_error) in [
        (
            Path::new("/"),
            false,
            false,
            Some("refusing to index filesystem root"),
        ),
        (Path::new("/"), true, false, None),
        (
            workspace.root(),
            false,
            false,
            Some("multi-repository container"),
        ),
        (workspace.root(), true, false, None),
        (workspace.root(), false, true, None),
    ] {
        if manifest {
            workspace.write("acdd.yaml", "limits:\n  allow_broad_root: true\n");
        }
        let result = scanner::check_root_scope(root, allow_broad);
        if let Some(message) = expected_error {
            assert!(result.unwrap_err().to_string().contains(message));
        } else {
            result.unwrap();
        }
    }
    for repo in ["repo1", "repo2"] {
        fs::remove_dir_all(workspace.path(repo)).unwrap();
    }

    workspace.write(
        "acdd.yaml",
        r#"
limits:
  max_files: 50
  max_file_bytes: 1000
  max_total_bytes: 50000
  allow_broad_root: true
"#,
    );

    let adapter = scanner::load_adapter(workspace.root(), None).unwrap();
    assert_eq!(adapter.limits.max_files, 50);
    assert_eq!(adapter.limits.max_file_bytes, 1000);
    assert_eq!(adapter.limits.max_total_bytes, 50000);
    assert!(adapter.limits.allow_broad_root);

    workspace.write(".git/config", "");

    enum Expected {
        Error(&'static str),
        SuccessFiles(usize),
    }

    let large = "x = 1\n".repeat(50);
    let boundary = "x".repeat(acdd_core::config::constants::DEFAULT_MAX_FILE_BYTES as usize);
    let oversized = format!("{boundary}x");
    let byte_files = [
        ("src/large.py", large.as_str()),
        ("src/small.py", "x = 1\n"),
    ];
    let boundary_files = [("src/boundary.py", boundary.as_str())];
    let oversized_files = [("src/oversized.py", oversized.as_str())];
    let cases = [
        ("configured byte limit", "limits:\n  max_file_bytes: 100\n", &byte_files[..], Expected::Error("file exceeds byte limit")),
        ("default byte boundary", "limits:\n  max_file_bytes: 5242880\n  max_total_bytes: 6000000\n  allow_broad_root: true\n", &boundary_files[..], Expected::SuccessFiles(1)),
        ("one byte over", "limits:\n  max_file_bytes: 5242880\n  max_total_bytes: 6000000\n  allow_broad_root: true\n", &oversized_files[..], Expected::Error("file exceeds byte limit")),
        (
            "configured file count limit",
            "limits:\n  max_files: 2\n  allow_broad_root: true\n",
            &[
                ("src/a.py", "x = 1\n"),
                ("src/b.py", "x = 1\n"),
                ("src/c.py", "x = 1\n"),
            ][..],
            Expected::Error("workspace exceeds file limit"),
        ),
        (
            "aggregate byte limit",
            "limits:\n  max_total_bytes: 6\n  allow_broad_root: true\n",
            &[("src/a.py", "abc\n"), ("src/b.py", "def\n")][..],
            Expected::Error("workspace exceeds total byte limit"),
        ),
        (
            "file count limit ignores unrecognized files",
            "limits:\n  max_files: 2\n  allow_broad_root: true\n",
            &[
                ("src/a.py", "x = 1\n"),
                ("src/b.py", "y = 2\n"),
                ("notes.txt", "not indexed\n"),
            ][..],
            Expected::SuccessFiles(2),
        ),
    ];

    for (name, config, files, expected) in cases {
        let _ = std::fs::remove_dir_all(workspace.path("src"));
        let _ = std::fs::remove_file(workspace.path("notes.txt"));
        workspace.write("acdd.yaml", config);
        for &(file_path, content) in files {
            workspace.write(file_path, content);
        }

        let canonical = scanner::canonical_root(workspace.root()).unwrap();
        let adapter = scanner::load_adapter(&canonical, None).unwrap();
        match (scanner::scan_with_adapter(&canonical, &adapter), expected) {
            (Err(err), Expected::Error(expected_msg)) => {
                assert!(
                    err.to_string().contains(expected_msg),
                    "{name}: expected error containing '{expected_msg}', got: {err}"
                );
            }
            (Ok(report), Expected::SuccessFiles(expected_count)) => {
                assert_eq!(
                    report.files, expected_count,
                    "{name}: expected {expected_count} files, got {}",
                    report.files
                );
            }
            (result, _) => {
                panic!("{name}: unexpected result: {result:?}");
            }
        }
    }
    fs::remove_dir_all(workspace.path("src")).unwrap();
    fs::remove_file(workspace.path("acdd.yaml")).unwrap();

    workspace.write(".git/config", "");
    let source = workspace.root().join("src");
    fs::create_dir_all(&source).unwrap();
    let mut created_paths = 0;
    for group_start in (0..=acdd_core::config::constants::DEFAULT_MAX_FILES).step_by(100) {
        let group_end =
            (group_start + 100).min(acdd_core::config::constants::DEFAULT_MAX_FILES + 1);
        let seed = workspace
            .root()
            .join(format!(".file-limit-seed-{group_start}.data"));
        fs::write(&seed, "x=1\n").unwrap();
        for index in group_start..group_end {
            // Distinct directory entries share fixture bytes in bounded groups.
            let path = source.join(format!("file_{index}.py"));
            match fs::hard_link(&seed, &path) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                    ) =>
                {
                    fs::write(path, "x=1\n").unwrap();
                }
                Err(error) => panic!("create distinct scanner fixture path: {error}"),
            }
            created_paths += 1;
        }
        fs::remove_file(seed).unwrap();
    }

    assert_eq!(acdd_core::config::constants::DEFAULT_MAX_FILES, 100_000);
    assert_eq!(created_paths, 100_001);
    assert_eq!(
        created_paths,
        acdd_core::config::constants::DEFAULT_MAX_FILES + 1
    );
    let canonical = scanner::canonical_root(workspace.root()).unwrap();
    let default_adapter = scanner::load_adapter(&canonical, None).unwrap();
    let error = scanner::scan_with_adapter(&canonical, &default_adapter)
        .map(|report| report.files)
        .expect_err("one path beyond the default limit must be rejected");
    let msg = error.to_string();
    let prefix = "workspace exceeds file limit (100001 > max_files 100000) at src/";
    assert!(msg.starts_with(prefix) && msg.len() > prefix.len(), "{msg}");
    fs::remove_dir_all(workspace.path("src")).unwrap();

    workspace.write("src/main.py", "def source(): pass\n");
    workspace.write("src/build_helpers.py", "def source(): pass\n");
    for directory in [
        ".git",
        ".acdd",
        ".venv",
        "vendor",
        "target",
        "node_modules",
        "build",
        "dist",
        "coverage",
        ".cache",
        ".next",
        ".nuxt",
        ".svelte-kit",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".gradle",
        "__pycache__",
    ] {
        workspace.write(format!("{directory}/generated.py"), "def source(): pass\n");
    }

    let canonical = scanner::canonical_root(workspace.root()).unwrap();
    let adapter = scanner::load_adapter(&canonical, None).unwrap();
    let report = scanner::scan_with_adapter(&canonical, &adapter).unwrap();
    let scanner::ColdScanResult {
        candidates: mut collected,
        dependency_manifests: manifests,
        report: collected_report,
    } = scanner::collect_for_build(workspace.root(), &adapter).unwrap();
    let mut streamed = Vec::new();
    let stream_report = scanner::stream_for_build(workspace.root(), &adapter, |candidate| {
        streamed.push(candidate);
        Ok(())
    })
    .unwrap();
    collected.sort_by(|a, b| a.path.cmp(&b.path));
    streamed.sort_by(|a, b| a.path.cmp(&b.path));
    for (left, right) in collected.iter().zip(&streamed) {
        assert_eq!(
            (
                &left.path,
                left.bytes,
                left.mtime_ns,
                left.identity,
                &left.language,
                left.is_doc
            ),
            (
                &right.path,
                right.bytes,
                right.mtime_ns,
                right.identity,
                &right.language,
                right.is_doc
            )
        );
    }
    assert_eq!(collected.len(), streamed.len());
    assert_eq!(
        (collected_report.files, collected_report.bytes),
        (stream_report.files, stream_report.bytes)
    );
    assert_eq!(
        manifests,
        scanner::dependency_manifests_for_build(workspace.root(), &adapter).unwrap()
    );
    let paths: Vec<_> = report
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["src/build_helpers.py", "src/main.py"]);

    let budget = acdd_core::sys::memory_budget_bytes();
    assert!(
        (64 * 1024 * 1024..=16 * 1024 * 1024 * 1024).contains(&budget),
        "budget must retain its 64 MiB floor and 16 GiB cap"
    );
}

#[test]
fn scanner_delta_policy_matrix() {
    let workspace = Workspace::new();
    workspace.write("repo-a/.git/config", "");
    workspace.write("repo-b/.git/config", "");
    let database = workspace.path(".acdd/code-map.sqlite");
    let adapter = workspace.path("adapter.yaml");
    for (name, limits, stale, expected_error) in [
        (
            "file bytes",
            "  max_file_bytes: 3\n  max_files: 10\n  max_total_bytes: 100000\n",
            false,
            Some("file exceeds byte limit"),
        ),
        ("broad root", "", false, None),
        ("stale explicit policy", "", true, None),
    ] {
        workspace.write("repo-a/src/a.py", "x");
        workspace.write("repo-b/src/b.py", "y");
        workspace.write(
            "adapter.yaml",
            &format!(
                "roots: [repo-a/src, repo-b/src]\nlimits:\n  allow_broad_root: true\n{limits}"
            ),
        );
        writer::build(workspace.root(), &database, Some(&adapter)).unwrap();
        let conn = Connection::open(&database).unwrap();
        let previous_digest: String = conn
            .query_row(
                "SELECT digest FROM source_inventory WHERE path='repo-a/src/a.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if stale {
            let raw: String = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='adapter'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let mut policy: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert!(policy.as_object_mut().unwrap().remove("limits").is_some());
            conn.execute(
                "UPDATE metadata SET value=?1 WHERE key='adapter'",
                [policy.to_string()],
            )
            .unwrap();
            conn.execute(
                "UPDATE metadata SET value='13' WHERE key='schema_version'",
                [],
            )
            .unwrap();
        }
        drop(conn);
        workspace.write("repo-a/src/a.py", "x = 3\n");
        let result = writer::delta_with_selection(
            workspace.root(),
            &database,
            &[PathBuf::from("repo-a/src/a.py")],
            None,
            None,
        );
        if let Some(message) = expected_error {
            let error = result.unwrap_err();
            assert!(error.to_string().contains(message), "{name}: {error:#}");
        } else {
            result.unwrap();
            let conn = Connection::open(&database).unwrap();
            let digest: String = conn
                .query_row(
                    "SELECT digest FROM source_inventory WHERE path='repo-a/src/a.py'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_ne!(digest, previous_digest, "{name}");
            let version: String = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(version, scanner::ENGINE_SCHEMA_VERSION);
            let raw: String = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key='adapter'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let policy: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(policy["adapter_path"], "adapter.yaml");
            assert_eq!(policy["limits"]["allow_broad_root"], true);
        }
    }
}
