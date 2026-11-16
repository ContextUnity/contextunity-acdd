use super::*;

#[test]
fn contract_seam_accepts_root_cargo_package_imports_from_integration_tests() {
    let root = prepare_indexed_boundary_workspace("pub fn route() -> usize { 1 }\n",
"",
"use crate::api::route;\n#[test]\nfn test_connected() { let actual = route(); assert_eq!(actual, 1); }\n",
"route",
None,
None,
"tests/connected.rs");
    let profile_path = root.root().join(".acdd/profile.yaml");
    let profile_before = std::fs::read(&profile_path).unwrap();
    root.write(
        "tests/connected.rs",
        "use boundary_fixture::api::route;\n#[test]\nfn integration_connected() { let actual = route(); assert_eq!(actual, 1); }\n",
    );
    commit_fixture_baseline(&root);
    root.build_to(root.path(".acdd/code-map.sqlite"));
    TaskHarness::claim_at(
        root.root(),
        "acdd/m-current-boundary:routing",
        "contract",
        "contract-author",
    )
    .unwrap();
    submit_indexed_boundary(&root, "contract", "tests/connected.rs").unwrap();
    assert_eq!(
        tasks::store_for_task(root.root(), "acdd/m-current-boundary:routing")
            .unwrap()
            .inspect("acdd/m-current-boundary:routing")
            .unwrap()
            .stage,
        "contract_review"
    );
    assert_eq!(std::fs::read(profile_path).unwrap(), profile_before);
}
#[test]
fn current_indexed_path_matrix() {
    assert_safe_git_environment();
    let api = "pub fn route() -> usize { 1 }\n";
    let caller = "use crate::api::route;\npub fn helper() -> usize { route() }\npub fn outer() -> usize { helper() }\npub fn wrapper() -> usize { outer() }\n";
    let direct = "use crate::api::route;\n#[test]\nfn test_connected() { let actual = route(); assert_eq!(actual, 1); }\n";
    let indirect = "use crate::caller::helper;\n#[test]\nfn test_connected() { let actual = helper(); assert_eq!(actual, 1); }\n";
    let local_alias = "use crate::api::route;\nuse crate::other::route as other_route;\n#[test]\nfn test_connected() { let handler = other_route; let handler = route; let actual = handler(); assert_eq!(actual, 1); }\n";
    let test_helper_source = "use crate::api::route;\npub fn helper() -> usize { route() }\n";
    let helper_seam = "use crate::test_helpers::helper;\n#[test]\nfn test_connected() { let actual = helper(); assert_eq!(actual, 1); }\n";
    let root = prepare_indexed_boundary_workspace(
        api,
        caller,
        helper_seam,
        "route",
        Some(test_helper_source),
        Some("helper"),
        "src/seam.rs::test_connected",
    );
    // Separate test files keep file-level references confined to the selected path.
    root.write("src/seam.rs", direct);
    root.write("src/two_hops.rs", indirect);
    root.write(
        "src/three_hops.rs",
        &direct
            .replace("api::route", "caller::outer")
            .replace("route()", "outer()"),
    );
    root.write(
        "src/four_hops.rs",
        &direct
            .replace("api::route", "caller::wrapper")
            .replace("route()", "wrapper()"),
    );
    root.write("src/alias_seam.rs", local_alias);
    root.write("src/helper_seam.rs", helper_seam);
    root.write("src/lib.rs", "pub mod api;\npub mod caller;\npub mod other;\npub mod test_helpers;\n#[cfg(test)] mod seam;\n#[cfg(test)] mod two_hops;\n#[cfg(test)] mod three_hops;\n#[cfg(test)] mod four_hops;\n#[cfg(test)] mod alias_seam;\n#[cfg(test)] mod helper_seam;\n");
    commit_fixture_baseline(&root);
    root.build_to(root.path(".acdd/code-map.sqlite"));
    let task_id = "acdd/m-current-boundary:routing";
    let baseline_task = tasks::store_for_task(root.root(), task_id)
        .unwrap()
        .inspect(task_id)
        .unwrap();
    assert_eq!(baseline_task.status, "ready");
    assert_eq!(baseline_task.stage, "contract");
    assert!(baseline_task.worker_id.is_none());
    let profile_seed = std::fs::read_to_string(root.path(".acdd/profile.yaml")).unwrap();
    let baseline_head = git_head(root.root());
    let snapshot = FixtureSnapshot::capture(&root);
    let baseline_database = std::fs::read(root.path(".acdd/tasks.sqlite")).unwrap();
    struct Case<'a> {
        name: &'a str,
        seam: &'a str,
        writes: Vec<(&'a str, String)>,
        refresh: bool,
        expected_error: Option<&'a str>,
    }
    let disconnected = "#[test]\nfn test_connected() { assert_eq!(1, 2); }\n";
    let stale = Some("TASK_SEAM_STALE");
    let cases = [
        Case {
            name: "fresh_direct",
            seam: "src/seam.rs::test_connected",
            writes: vec![],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "fresh_two_hops",
            seam: "src/two_hops.rs::test_connected",
            writes: vec![],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "three_hops",
            seam: "src/three_hops.rs::test_connected",
            writes: vec![],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "four_hops",
            seam: "src/four_hops.rs::test_connected",
            writes: vec![],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "removed_direct_call",
            seam: "src/seam.rs::test_connected",
            writes: vec![("src/seam.rs", disconnected.into())],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "removed_direct_call_file",
            seam: "src/seam.rs",
            writes: vec![("src/seam.rs", disconnected.into())],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "disconnected_helper",
            seam: "src/two_hops.rs::test_connected",
            writes: vec![(
                "src/caller.rs",
                "pub fn helper() -> usize { 2 }\npub fn outer() -> usize { helper() }\n".into(),
            )],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "deleted_helper",
            seam: "src/two_hops.rs::test_connected",
            writes: vec![("src/caller.rs", "pub fn outer() -> usize { 2 }\n".into())],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "deleted_target",
            seam: "src/seam.rs::test_connected",
            writes: vec![("src/api.rs", "pub fn replacement() -> usize { 1 }\n".into())],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "unindexed_new_test",
            seam: "src/seam.rs",
            writes: vec![(
                "src/seam.rs",
                format!(
                    "{disconnected}{}",
                    direct.replace("test_connected", "test_new")
                ),
            )],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "refreshed_disconnected",
            seam: "src/seam.rs::test_connected",
            writes: vec![("src/seam.rs", disconnected.into())],
            refresh: true,
            expected_error: Some("TASK_SEAM_DISCONNECTED"),
        },
        Case {
            name: "refreshed_connected",
            seam: "src/seam.rs::test_connected",
            writes: vec![(
                "src/seam.rs",
                direct
                    .replace("assert_eq!", "assert_ne!")
                    .replace(", 1)", ", 2)"),
            )],
            refresh: true,
            expected_error: None,
        },
        Case {
            name: "unrelated_file_change",
            seam: "src/seam.rs::test_connected",
            writes: vec![(
                "src/unrelated.rs",
                "pub fn unrelated() -> usize { 2 }\n".into(),
            )],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "retargeted_import",
            seam: "src/seam.rs::test_connected",
            writes: vec![
                (
                    "src/seam.rs",
                    direct.replace("use crate::api::route;", "use crate::other::route;"),
                ),
                ("src/other.rs", "pub fn route() -> usize { 2 }\n".into()),
            ],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "retargeted_local_alias",
            seam: "src/alias_seam.rs::test_connected",
            writes: vec![(
                "src/alias_seam.rs",
                local_alias.replace("let handler = route", "let handler = other_route"),
            )],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "target_becomes_test",
            seam: "src/seam.rs::test_connected",
            writes: vec![(
                "src/api.rs",
                "#[test]\npub fn route() -> usize { 1 }\n".into(),
            )],
            refresh: false,
            expected_error: stale,
        },
        Case {
            name: "line_shift_only",
            seam: "src/seam.rs::test_connected",
            writes: vec![("src/seam.rs", format!("// shifted line\n{direct}"))],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "fresh_test_marked_helper",
            seam: "src/helper_seam.rs::test_connected",
            writes: vec![],
            refresh: false,
            expected_error: None,
        },
        Case {
            name: "stale_test_marked_helper",
            seam: "src/helper_seam.rs::test_connected",
            writes: vec![(
                "src/test_helpers.rs",
                test_helper_source.replace(
                    "pub fn helper() -> usize { route() }",
                    "fn helper() -> usize { 1 }",
                ),
            )],
            refresh: false,
            expected_error: stale,
        },
    ];
    let baseline_refs = TaskHarness::references(root.root(), "refs/acdd/");
    assert!(baseline_refs.status.success());
    let baseline_refs: Vec<_> = String::from_utf8(baseline_refs.stdout)
        .unwrap()
        .lines()
        .map(|name| {
            (
                name.to_owned(),
                TaskHarness::revision_text(root.root(), name).unwrap(),
            )
        })
        .collect();
    let mut failures = Vec::new();
    let mut previous_clean = None;
    for case in cases
        .iter()
        .filter(|case| case.writes.is_empty() && !case.refresh)
        .chain(
            cases
                .iter()
                .filter(|case| !case.writes.is_empty() || case.refresh),
        )
    {
        match previous_clean {
            None => {} // Capture already left the fixture at its baseline.
            Some(true) => {
                // A successful submission changes task state and acdd refs even
                // when the indexed source and worktree remain untouched.
                let refs = TaskHarness::references(root.root(), "refs/acdd/");
                assert!(refs.status.success());
                for name in String::from_utf8(refs.stdout).unwrap().lines() {
                    if !baseline_refs.iter().any(|(baseline, _)| baseline == name) {
                        assert!(TaskHarness::delete_reference(root.root(), name)
                            .status
                            .success());
                    }
                }
                for (name, commit) in &baseline_refs {
                    assert!(
                        TaskHarness::update_reference(root.root(), name, commit, None)
                            .status
                            .success()
                    );
                }
                for suffix in ["-wal", "-shm", "-journal"] {
                    let path = root.path(format!(".acdd/tasks.sqlite{suffix}"));
                    if path.exists() {
                        std::fs::remove_file(path).unwrap();
                    }
                }
                std::fs::write(root.path(".acdd/tasks.sqlite"), &baseline_database).unwrap();
            }
            Some(false) => snapshot.restore(&root),
        }
        assert_eq!(
            git_head(root.root()),
            baseline_head,
            "{}: fixture HEAD ref",
            case.name
        );
        assert_eq!(
            std::fs::read(root.path(".acdd/tasks.sqlite")).unwrap(),
            baseline_database,
            "{}: task database should return to the shared seed",
            case.name
        );
        let restored = tasks::store_for_task(root.root(), task_id)
            .unwrap()
            .inspect(task_id)
            .unwrap();
        assert_eq!(restored.status, baseline_task.status);
        assert_eq!(restored.stage, baseline_task.stage);
        assert_eq!(restored.claim_revision, baseline_task.claim_revision);
        assert_eq!(restored.worker_id, baseline_task.worker_id);
        root.write(
            ".acdd/profile.yaml",
            &profile_seed.replace("src/seam.rs::test_connected", case.seam),
        );
        TaskHarness::claim_at(root.root(), task_id, "contract", "contract-author").unwrap();
        for (path, source) in &case.writes {
            root.write(path, source);
        }
        if case.refresh {
            root.build_to(root.path(".acdd/code-map.sqlite"));
        }
        let result = submit_indexed_boundary(&root, "contract", case.seam);
        match (case.expected_error, result) {
            (Some(expected), Err(error)) => {
                let error = error.to_string();
                if !error.contains(expected) {
                    failures.push(format!("{}: expected {expected}, got: {error}", case.name));
                }
                for diagnostic in [
                    "Calls discovered:",
                    "Coverage statuses:",
                    "Reached targets:",
                    "Stop reason:",
                ] {
                    if !error.contains(diagnostic) {
                        failures.push(format!(
                            "{}: missing '{diagnostic}' diagnostic: {error}",
                            case.name
                        ));
                    }
                }
                if expected == "TASK_SEAM_STALE" && !error.contains("acdd scan") {
                    failures.push(format!(
                        "{}: stale diagnostic omits refresh action: {error}",
                        case.name
                    ));
                }
                assert_eq!(
                    tasks::store_for_task(root.root(), task_id)
                        .unwrap()
                        .inspect(task_id)
                        .unwrap()
                        .stage,
                    "contract"
                );
            }
            (Some(code), Ok(_)) => failures.push(format!(
                "{}: accepted a seam without a current indexed production path ({code})",
                case.name
            )),
            (None, Err(error)) => failures.push(format!(
                "{}: valid current indexed seam rejected: {error}",
                case.name
            )),
            (None, Ok(_)) => {}
        }
        previous_clean = Some(case.writes.is_empty() && !case.refresh);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn contract_seam_validates_claimed_worktree_against_shared_primary_index() {
    let api = "pub fn route() -> usize { 1 }\n";
    let caller = "use crate::api::route;\npub fn helper() -> usize { route() }\n";
    let indirect = "use crate::caller::helper;\n#[test]\nfn test_connected() { let actual = helper(); assert_eq!(actual, 1); }\n";
    let root = prepare_indexed_boundary_workspace(
        api,
        caller,
        indirect,
        "route",
        None,
        None,
        "src/seam.rs::test_connected",
    );
    let mut failures = Vec::new();
    let worktree = root.root().join("claimed-worktree");
    let added = TaskHarness::create_worktree(root.root(), &worktree, "claimed-boundary");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    TaskHarness::claim_in_worktree(
        root.root(),
        "acdd/m-current-boundary:routing",
        "contract",
        "contract-author",
        &worktree,
        Some(&TaskHarness::session_for("contract-author")),
    )
    .unwrap();
    for (case, stale_file) in [
        ("stale_test", "src/seam.rs"),
        ("stale_helper", "src/caller.rs"),
    ] {
        if stale_file == "src/caller.rs" {
            std::fs::write(worktree.join("src/seam.rs"), indirect).unwrap();
            std::fs::write(
                worktree.join(stale_file),
                "pub fn helper() -> usize { 2 }\n",
            )
            .unwrap();
        } else {
            std::fs::write(
                worktree.join(stale_file),
                "#[test]\nfn test_connected() { assert_eq!(1, 2); }\n",
            )
            .unwrap();
        }
        contextunity_acdd::engine::pipeline::build_with_cache_dir(
            &worktree,
            &worktree.join(".acdd/code-map.sqlite"),
            None,
            &worktree.join(".acdd/extraction-cache"),
        )
        .unwrap();
        match submit_indexed_boundary(&root, "contract", "src/seam.rs::test_connected") {
            Err(error) => {
                assert!(
                    error.to_string().contains("TASK_SEAM_DISCONNECTED"),
                    "{case}: {error}"
                );
                assert_eq!(
                    tasks::store_for_task(root.root(), "acdd/m-current-boundary:routing")
                        .unwrap()
                        .inspect("acdd/m-current-boundary:routing")
                        .unwrap()
                        .stage,
                    "contract"
                );
            }
            Ok(_) => failures.push(format!(
                "{case}: shared primary index attested a disconnected claimed worktree despite fresh local index"
            )),
        }
    }
    let removed = TaskHarness::remove_worktree(root.root(), &worktree);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
