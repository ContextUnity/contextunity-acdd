use super::*;

#[test]
fn trait_signature_matrix() {
    assert_safe_git_environment();
    let cases = [
        (
            "default_trait_method",
            "pub trait Router { fn route(&self) -> usize { 1 } }",
            "pub trait Router { fn route(&self) -> u64 { 1 } }",
            true,
        ),
        (
            "trait_body_only",
            "pub trait Router { fn route(&self) -> usize { 1 } }",
            "pub trait Router { fn route(&self) -> usize { 2 } }",
            false,
        ),
        (
            "declared_trait_method",
            "pub trait Router { fn route(&self) -> usize; }",
            "pub trait Router { fn route(&self) -> u64; }",
            true,
        ),
        (
            "private_inherent_method",
            "pub trait Router { fn route(&self) -> usize { 1 } }\npub struct Local; impl Local { fn private_route(&self) -> usize { 1 } }",
            "pub trait Router { fn route(&self) -> usize { 1 } }\npub struct Local; impl Local { fn private_route(&self, value: usize) -> usize { value } }",
            false,
        ),
        (
            "trait_impl_body_only",
            "pub trait Router { fn route(&self) -> usize; }\npub struct Local; impl Router for Local { fn route(&self) -> usize { 1 } }",
            "pub trait Router { fn route(&self) -> usize; }\npub struct Local; impl Router for Local { fn route(&self) -> usize { 2 } }",
            false,
        ),
        (
            "crate_visible_trait",
            "pub(crate) trait Router { fn route(&self) -> usize { 1 } }",
            "pub(crate) trait Router { fn route(&self) -> u64 { 1 } }",
            true,
        ),
        (
            "qualified_visible_trait",
            "pub(in crate) trait Router { fn route(&self) -> usize { 1 } }",
            "pub(in crate) trait Router { fn route(&self) -> u64 { 1 } }",
            true,
        ),
    ];
    let anchor = "pub fn anchor() -> usize { 1 }\n";
    let connected = "use crate::api::anchor;\n#[test]\nfn test_connected() { let actual = anchor(); assert_eq!(actual, 1); }\n";
    let mut api = anchor.to_owned();
    let mut caller = String::new();
    for (case, baseline, _, _) in cases {
        api.push_str(&format!("pub mod {case} {{ {baseline} }}\n"));
        caller.push_str(&format!("pub fn invoke_route_{case}(router: &dyn crate::api::{case}::Router) -> usize {{ crate::api::{case}::Router::route(router) }}\n"));
    }
    let root = indexed_boundary_workspace(
        &api,
        &caller,
        connected,
        "anchor",
        "src/seam.rs::test_connected",
    );
    let stages: Vec<String> = {
        let harness = TaskHarness::attach(&root, "acdd/m-current-boundary:routing");
        harness
            .executable_gates()
            .unwrap()
            .into_iter()
            .filter(|gate| {
                matches!(
                    gate.proof.kind(),
                    Some(contextunity_acdd::core::tasks::profile::ProofKind::Contract)
                        | Some(contextunity_acdd::core::tasks::profile::ProofKind::Command)
                )
            })
            .map(|gate| gate.id)
            .collect()
    };
    let mut snapshots = HashMap::new();
    for (index, stage) in stages.iter().enumerate() {
        snapshots.insert(stage.clone(), FixtureSnapshot::capture(&root));
        if index + 1 == stages.len() {
            break;
        }
        submit_indexed_boundary(&root, stage, "src/seam.rs::test_connected").unwrap();
        TaskHarness::attach(&root, "acdd/m-current-boundary:routing")
            .advance_to(&stages[index + 1])
            .unwrap();
        TaskHarness::claim_at(
            root.root(),
            "acdd/m-current-boundary:routing",
            &stages[index + 1],
            "builder",
        )
        .unwrap();
    }
    let mut failures = Vec::new();
    for (case, baseline, changed, reject) in cases {
        for (stage, snapshot) in &snapshots {
            snapshot.restore(&root);
            let exact_calls = TaskHarness::external_method_caller_edges(
                root.root(),
                ("src/api.rs", "route"),
                ("src/caller.rs", &format!("invoke_route_{case}")),
            )
            .unwrap();
            assert!(
                exact_calls > 0,
                "{case}/{stage}: fixture must persist the exact external UFCS call"
            );
            root.write(
                "src/api.rs",
                &api.replace(
                    &format!("pub mod {case} {{ {baseline} }}"),
                    &format!("pub mod {case} {{ {changed} }}"),
                ),
            );
            let result = submit_indexed_boundary(&root, stage, "src/seam.rs::test_connected");
            match (reject, result) {
                (true, Err(error)) => {
                    assert!(error.to_string().contains("TASK_SCOPE_INCOMPLETE"), "{case}/{stage}: {error}");
                    assert!(error.to_string().contains("src/caller.rs"), "{case}/{stage}: {error}");
                    assert_eq!(tasks::store_for_task(root.root(), "acdd/m-current-boundary:routing").unwrap().inspect("acdd/m-current-boundary:routing").unwrap().stage, *stage);
                }
                (true, Ok(_)) => failures.push(format!("{case}/{stage}: changed implicitly public signature accepted despite exact external UFCS caller")),
                (false, Err(error)) => failures.push(format!("{case}/{stage}: unchanged public signature rejected: {error}")),
                (false, Ok(_)) => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn accepted_signature_baseline_matrix() {
    assert_safe_git_environment();
    let routing_baseline = r#"pub fn route_task() -> usize { 1 }
pub struct Router;
impl Router {
    pub fn route_task(&self) -> usize { 10 }
}
pub struct Archive;
impl Archive {
    pub fn route_task(&self) -> usize { 20 }
}
pub fn test_only_route() -> usize { 3 }
#[cfg(test)]
mod tests {
    use crate::routing::route_task;

    #[test]
    fn test_connected() {
        let connected_target = route_task();
        let router = crate::routing::Router;
        let actual = router.route_task();
        assert_eq!(connected_target, 1);
        assert_eq!(actual, 10);
    }
}
"#;
    let caller_baseline = "use crate::routing::Router;\nimpl Router {\n    pub fn invoke_route(&self) -> usize { self.route_task() }\n}\n";
    let connected_test_baseline = "#[test]\nfn test_connected() { let router = tiered_fixture::routing::Router; let actual = router.route_task(); assert_eq!(actual, 10); }\n";

    let root = acdd_workspace("src/routing.rs::test_connected", "");
    root.write(
        "src/lib.rs",
        "pub mod routing;\npub mod caller;\npub mod api;\npub mod client;\n",
    );
    root.write("src/routing.rs", routing_baseline);
    root.write("src/caller.rs", caller_baseline);
    root.write(
        "Cargo.toml",
        "[package]\nname = \"tiered_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    root.write("tests/connected.rs", connected_test_baseline);
    root.write("tests/acdd.rs", "mod tasks;\n");
    root.write(
        "tests/acdd/tasks.rs",
        "#[test]\nfn task_suite_is_present() { assert_eq!(1, 1); }\n",
    );
    root.write("AGENTS.md", "# Instructions\nLoad the acdd skill.\n");
    root.write(".agents/skills/acdd/SKILL.md", "# ContextUnity ACDD\n");
    root.write(
        "acdd.yaml",
        "roots: [src, tests]\ndocs: [docs]\ntasks_db: .acdd/tasks.sqlite\n",
    );
    root.write(
            "docs/milestones/010-tiered.md",
            "---\nid: m-tiered-signature\ntitle: Tiered signature boundary\ndoc_type: contract\nstatus: active\n---\n```yaml\ntask_ref: route-task\ntarget: Protect the exact public method signature\nproof_policy: seam-test-first\nscope: [src/routing.rs, tests/connected.rs, tests/acdd/tasks.rs]\nscope_roots: [src/, tests/]\n```\n",
        );
    root.write(
        "src/api.rs",
        "pub fn fetch(id: usize) -> usize { id }\n#[cfg(test)]\nmod tests { use crate::api::fetch; #[test] fn test_connected() { let actual = fetch(1); assert_eq!(actual, 1); } }\n",
    );
    root.write(
        "src/client.rs",
        "use crate::api::fetch;\npub fn load() -> usize { fetch(0) }\n",
    );
    let fetch_baseline = std::fs::read_to_string(root.path("src/api.rs")).unwrap();
    let fetch_test = "use tiered_fixture::api::fetch;\n#[test]\nfn test_fetch() { let actual = fetch(1); assert_eq!(actual, 1); }\n";
    root.write("tests/fetch.rs", fetch_test);
    root.write(
        "docs/milestones/010-tiered-rename.md",
        "---\nid: m-tiered-rename\ntitle: Renamed API boundary\ndoc_type: contract\nstatus: active\n---\n```yaml\ntask_ref: rename-fetch\ntarget: Preserve every caller when renaming a public API\nproof_policy: seam-test-first\nscope: [src/api.rs, tests/fetch.rs, tests/acdd/tasks.rs]\nscope_roots: [src/, tests/]\n```\n",
    );
    commit_fixture_baseline(&root);
    root.build_to(root.path(".acdd/code-map.sqlite"));
    let profile_seed = std::fs::read_to_string(root.path(".acdd/profile.yaml")).unwrap();
    let method_id = "acdd/m-tiered-signature:route-task";
    let rename_id = "acdd/m-tiered-rename:rename-fetch";
    for (manifest, task_id, seam, api_path, name, kind, caller_path, caller_name) in [
        (
            "docs/milestones/010-tiered.md",
            method_id,
            "src/routing.rs::test_connected",
            "src/routing.rs",
            "route_task",
            "method",
            "src/caller.rs",
            Some("invoke_route"),
        ),
        (
            "docs/milestones/010-tiered-rename.md",
            rename_id,
            "src/api.rs::test_connected",
            "src/api.rs",
            "fetch",
            "function",
            "src/client.rs",
            None,
        ),
    ] {
        root.write(
            ".acdd/profile.yaml",
            &profile_seed.replace("src/routing.rs::test_connected", seam),
        );
        assert!(
            boundary_caller_edge_count(&root, kind, api_path, name, &[caller_path], caller_name)
                > 0,
            "baseline index must include the exact external caller to {api_path}::{name}"
        );
        tasks::manage(
            root.root(),
            serde_json::from_value(json!({"action":"sync","milestone_ref":manifest})).unwrap(),
        )
        .unwrap();
        TaskHarness::claim_at(root.root(), task_id, "contract", "contract-author").unwrap();
        let task = tasks::store_for_task(root.root(), task_id)
            .unwrap()
            .inspect(task_id)
            .unwrap();
        let mut contract = TaskHarness::default_evidence(&task);
        contract.proof = json!({"contract_proof":{"seam_test_ref":seam,"red_exit_code":101}});
        tasks::submit(
            root.root(),
            TaskHarness::pass_request(task_id, "contract", &contract),
        )
        .unwrap();
        let accepted = TaskHarness::latest_submission_result(
            &tasks::store_for_task(root.root(), task_id).unwrap(),
            task_id,
        )
        .unwrap()
        .1;
        assert!(
            accepted["boundary_callers"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry["path"] == api_path
                        && entry["name"] == name
                        && entry["caller_paths"]
                            .as_array()
                            .is_some_and(|paths| paths.iter().any(|path| path == caller_path))
                }),
            "contract acceptance must persist the stable-baseline external caller path"
        );
        let baseline = TaskHarness::revision(root.root(), "HEAD");
        assert!(baseline.status.success());
        assert_eq!(
            accepted["boundary_callers"]["baseline_commit"],
            String::from_utf8_lossy(&baseline.stdout).trim(),
            "caller evidence must be pinned to the Git baseline accepted by the contract gate"
        );
        TaskHarness::attach(&root, task_id)
            .advance_to("build")
            .unwrap();
        let claim = TaskHarness::claim_at(root.root(), task_id, "build", "builder").unwrap();
        assert_eq!(claim["stage"], "build");
    }
    let original_caller_edges = boundary_caller_edge_count(
        &root,
        "method",
        "src/routing.rs",
        "route_task",
        &["src/caller.rs"],
        Some("invoke_route"),
    );
    let snapshot = FixtureSnapshot::capture(&root);
    let method_signature = "impl Router {\n    pub fn route_task(&self) -> usize { 10 }";
    let changed_signature =
        "impl Router {\n    pub fn route_task(&self, value: usize) -> usize { 10 + value }";
    let body_signature = "impl Router {\n    pub fn route_task(&self) -> usize { 11 }";
    struct Case<'a> {
        name: &'a str,
        task_id: &'a str,
        writes: Vec<(&'a str, String)>,
        expected_error: Option<&'a str>,
        caller_path: &'a str,
        refresh: bool,
        renamed: bool,
        expected_method: &'a str,
    }
    let cases = [
        Case {
            name: "same_named_methods",
            task_id: method_id,
            writes: vec![
                (
                    "src/routing.rs",
                    routing_baseline.replace(method_signature, changed_signature),
                ),
                (
                    "src/caller.rs",
                    caller_baseline.replace("self.route_task()", "self.route_task(0)"),
                ),
                (
                    "tests/connected.rs",
                    connected_test_baseline.replace("router.route_task()", "router.route_task(0)"),
                ),
            ],
            expected_error: Some("TASK_SCOPE_INCOMPLETE"),
            caller_path: "src/caller.rs",
            refresh: false,
            renamed: false,
            expected_method: changed_signature,
        },
        Case {
            name: "body_only",
            task_id: method_id,
            writes: vec![
                (
                    "src/routing.rs",
                    routing_baseline.replace(method_signature, body_signature),
                ),
                (
                    "tests/connected.rs",
                    connected_test_baseline
                        .replace("assert_eq!(actual, 10)", "assert_eq!(actual, 11)"),
                ),
            ],
            expected_error: None,
            caller_path: "src/caller.rs",
            refresh: false,
            renamed: false,
            expected_method: body_signature,
        },
        Case {
            name: "refreshed_index",
            task_id: method_id,
            writes: vec![
                (
                    "src/routing.rs",
                    routing_baseline.replace(method_signature, changed_signature),
                ),
                (
                    "src/caller.rs",
                    caller_baseline.replace("self.route_task()", "self.route_task(0)"),
                ),
                (
                    "tests/connected.rs",
                    connected_test_baseline.replace("router.route_task()", "router.route_task(0)"),
                ),
            ],
            expected_error: Some("TASK_SCOPE_INCOMPLETE"),
            caller_path: "src/caller.rs",
            refresh: true,
            renamed: false,
            expected_method: changed_signature,
        },
        Case {
            name: "renamed_function",
            task_id: rename_id,
            writes: vec![
                ("src/api.rs", fetch_baseline.replace("fetch", "fetch_by_id")),
                ("tests/fetch.rs", fetch_test.replace("fetch", "fetch_by_id")),
            ],
            expected_error: Some("TASK_SCOPE_INCOMPLETE"),
            caller_path: "src/client.rs",
            refresh: true,
            renamed: true,
            expected_method: method_signature,
        },
    ];
    for case in cases {
        snapshot.restore(&root);
        assert_eq!(
            boundary_caller_edge_count(
                &root,
                "method",
                "src/routing.rs",
                "route_task",
                &["src/caller.rs"],
                Some("invoke_route")
            ),
            original_caller_edges
        );
        for (path, source) in case.writes {
            root.write(path, &source);
        }
        let routing = std::fs::read_to_string(root.path("src/routing.rs")).unwrap();
        assert!(
            routing.contains("pub fn route_task() -> usize { 1 }"),
            "{}: same-named free function",
            case.name
        );
        assert!(
            routing.contains("impl Archive {\n    pub fn route_task(&self) -> usize { 20 }"),
            "{}: unchanged Archive method",
            case.name
        );
        assert!(
            routing.contains(case.expected_method),
            "{}: exact qualified Router signature/body",
            case.name
        );
        if !case.renamed {
            assert_ne!(
                routing, routing_baseline,
                "{}: Router method must change",
                case.name
            );
        }
        if case.refresh {
            root.build_to(root.path(".acdd/code-map.sqlite"));
            if case.renamed {
                assert_eq!(boundary_caller_edge_count(&root, "function", "src/api.rs", "fetch", &["src/client.rs", "src/client.ts", "src/client.js", "src/client.py"], None), 0,
                    "reindex must remove every edge to the renamed fetch symbol before boundary validation");
            } else {
                let refreshed = boundary_caller_edge_count(
                    &root,
                    "method",
                    "src/routing.rs",
                    "route_task",
                    &["src/caller.rs"],
                    Some("invoke_route"),
                );
                assert!(refreshed >= original_caller_edges, "reindex must preserve external Router caller edges: before={original_caller_edges}, after={refreshed}");
            }
        }
        let task = tasks::store_for_task(root.root(), case.task_id)
            .unwrap()
            .inspect(case.task_id)
            .unwrap();
        let mut build = TaskHarness::default_evidence(&task);
        build.proof = json!({"command_proof":{"command":"rust-check-and-test","exit_code":0,"tests_passed":1,"tests_failed":0}});
        let result = tasks::submit(
            root.root(),
            TaskHarness::pass_request(case.task_id, "build", &build),
        );
        match (case.expected_error, result) {
            (Some(code), Err(error)) => {
                assert!(error.to_string().contains(code), "{}: {error}", case.name);
                assert!(
                    error.to_string().contains(case.caller_path),
                    "{}: saved external caller must explain rejection: {error}",
                    case.name
                );
            }
            (Some(code), Ok(_)) => panic!("{}: expected rejection with {code}", case.name),
            (None, Err(error)) => panic!("{}: body-only edit must pass: {error}", case.name),
            (None, Ok(_)) => {}
        }
    }
}
#[test]
fn signature_boundary_covers_public_callables_across_languages() {
    assert_safe_git_environment();
    let root = acdd_workspace("src/routing.rs::test_connected", "");
    root.write("src/lib.rs", "pub mod routing;\n");
    root.write("src/routing.rs", "pub fn route_task() -> usize { 1 }\n#[cfg(test)]\nmod tests { use crate::routing::route_task; #[test] fn test_connected() { let actual = route_task(); assert_eq!(actual, 1); } }\n");
    root.write(
        "src/api_typescript.ts",
        "export function submit(id: number): number { return id; }\n",
    );
    root.write(
        "src/client.ts",
        "import { submit } from './api_typescript';\nexport function load(): number { return submit(1); }\n",
    );
    root.write(
        "src/api_python.py",
        "def fetch(id: int) -> int:\n    return id\n\ndef test_connected():\n    actual = fetch(1)\n    assert actual == 1\n",
    );
    root.write(
        "src/client.py",
        "from api_python import fetch\n\ndef load():\n    return fetch(0)\n",
    );
    root.write(
        "src/api_javascript.js",
        "export function submit(id) { return id; }\n",
    );
    root.write(
        "src/client.js",
        "import { submit } from './api_javascript.js';\nexport function load() { return submit(1); }\n",
    );
    root.write(
        "Cargo.toml",
        "[package]\nname = \"tiered_callables_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    root.write("tests/connected.rs", "use tiered_callables_fixture::routing::route_task;\n#[test]\nfn test_connected() { let actual = route_task(); assert_eq!(actual, 1); }\n");
    root.write("tests/acdd.rs", "mod tasks;\n");
    root.write(
        "tests/acdd/tasks.rs",
        "#[test]\nfn task_suite_is_present() { assert_eq!(1, 1); }\n",
    );
    root.write("AGENTS.md", "# Instructions\nLoad the acdd skill.\n");
    root.write(".agents/skills/acdd/SKILL.md", "# ContextUnity ACDD\n");
    root.write(
        "acdd.yaml",
        "roots: [src, tests]\ndocs: [docs]\ntasks_db: .acdd/tasks.sqlite\n",
    );
    let milestone_ref = "docs/milestones/010-tiered-callables.md";
    root.write(
        milestone_ref,
        "---\nid: m-tiered-callables\ntitle: Public callable signature boundaries\ndoc_type: contract\nstatus: active\n---\n```yaml\ntask_ref: protect-callables\ntarget: Protect public callable signatures across supported languages\nproof_policy: seam-test-first\nscope: [src/routing.rs, src/api_typescript.ts, src/api_python.py, src/api_javascript.js, tests/connected.rs]\nscope_roots: [src/, tests/]\n```\n",
    );
    commit_fixture_baseline(&root);
    root.build_to(root.path(".acdd/code-map.sqlite"));
    let callable_cases = [
        (
            "TypeScript",
            "src/api_typescript.ts",
            "submit",
            "src/client.ts",
            "export function submit(id: string): number { return id.length; }\n",
            "baseline index must resolve the TypeScript external caller",
            "reindex must retain the caller edge to the same exported TypeScript callable",
            "exported TypeScript API changes must consult indexed external callers",
        ),
        (
            "Python",
            "src/api_python.py",
            "fetch",
            "src/client.py",
            "def fetch(id: str) -> int:\n    return len(id)\n\ndef test_connected():\n    actual = fetch(\"1\")\n    assert actual == 1\n",
            "baseline index must resolve the Python external caller",
            "reindex must retain the caller edge to the same public Python callable",
            "public Python API changes must consult indexed external callers",
        ),
        (
            "JavaScript",
            "src/api_javascript.js",
            "submit",
            "src/client.js",
            "export function submit(id, label) { return id; }\n",
            "baseline index must resolve the JavaScript external caller",
            "reindex must retain the caller edge to the same exported JavaScript callable",
            "exported JavaScript API changes must consult indexed external callers",
        ),
    ];
    // Check baseline calls before any snapshot restore; each mutation row also
    // verifies the production index after reindexing below.
    for (language, api_path, callable_name, caller_path, _, baseline_expectation, _, _) in
        callable_cases
    {
        assert!(
            boundary_caller_edge_count(
                &root,
                "function",
                api_path,
                callable_name,
                &[caller_path],
                None,
            ) > 0,
            "{language}: {baseline_expectation}"
        );
    }
    tasks::manage(
        root.root(),
        tasks::Manage {
            action: tasks::ManageAction::Sync,
            milestone_ref: Some(milestone_ref.to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let task_id = "acdd/m-tiered-callables:protect-callables";
    {
        let mut h = TaskHarness::attach(&root, task_id);
        h.advance_to("build").unwrap();
        h.claim_next().unwrap();
    }
    let snapshot = FixtureSnapshot::capture(&root);

    for (
        language,
        api_path,
        callable_name,
        caller_path,
        changed_api,
        _,
        reindex_expectation,
        rejection_expectation,
    ) in callable_cases
    {
        snapshot.restore(&root);
        root.write(api_path, changed_api);
        root.build_to(root.path(".acdd/code-map.sqlite"));
        assert!(
            boundary_caller_edge_count(
                &root,
                "function",
                api_path,
                callable_name,
                &[caller_path],
                None,
            ) > 0,
            "{language}: {reindex_expectation}"
        );
        let task = tasks::store_for_task(root.root(), task_id)
            .unwrap()
            .inspect(task_id)
            .unwrap();
        let mut build = TaskHarness::default_evidence(&task);
        build.proof = json!({"command_proof":{"command":"rust-check-and-test","exit_code":0,"tests_passed":1,"tests_failed":0}});
        let error = tasks::submit(
            root.root(),
            TaskHarness::pass_request(task_id, "build", &build),
        )
        .expect_err(rejection_expectation);
        assert!(
            error.to_string().contains("TASK_SCOPE_INCOMPLETE"),
            "{language}: {error}"
        );
        assert!(
            error.to_string().contains(caller_path),
            "{language}: external callable caller must explain the rejection: {error}"
        );
    }
}
