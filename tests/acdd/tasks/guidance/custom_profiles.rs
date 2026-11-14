use super::*;

#[test]
fn inherited_default_task_claims_validate_proofs_across_lifecycle() {
    use contextunity_forge_mcp::db::tasks_store::GateTransition;

    for seam in [false, true] {
        let workspace = TaskHarness::runner_workspace(DEFAULT_SEAM, "");
        let commands = FIXTURE_COMMANDS.split("gates:").next().unwrap();
        let profile = format!("inherit: default\n{commands}\ngates:\n  - tasks:\n      gates:\n        - id: contract\n          command: test\n        - id: build\n          command: rust-check-and-test\n        - id: seam\n          gates:\n            - id: integrate\n              command: rust-check-and-test\n");
        workspace.write(".forge/acdd/profile.yaml", &profile);
        workspace.write("src/lib.rs", "pub fn work() {}\n");
        workspace.write(
            "forge-mcp.yaml",
            "roots: [src]\ndocs: [docs]\ntasks_db: .forge/tasks.sqlite\n",
        );
        workspace.write(".gitignore", CANONICAL_FORGE_GITIGNORE);
        let mut builder = MilestoneFixtureBuilder::new("m-default-parity")
            .status("active")
            .task("case", &["src/lib.rs"]);
        if seam {
            builder = builder.task_concurrency_mode("parallel");
        }
        let manifest = builder.build();
        let path = "docs/milestones/010-default-parity.md";
        workspace.write(path, &manifest);
        TaskHarness::commit_handoff_fixture_inputs(
            &workspace,
            &[
                "src/lib.rs",
                "forge-mcp.yaml",
                ".gitignore",
                ".forge/acdd/profile.yaml",
                path,
            ],
        );
        let milestone = Milestone::parse(&manifest, "forge-mcp").unwrap();
        let id = milestone.task_id(&milestone.tasks[0]);
        let mut store = tasks::store_for_task(workspace.root(), &id).unwrap();
        store.sync(&milestone, path, workspace.root()).unwrap();
        for gate in [
            "blueprint_synthesis.synthesize",
            "blueprint_synthesis.review",
        ] {
            store
                .record_gate_transition(&GateTransition {
                    milestone_id: "forge-mcp/forge-mcp/m-default-parity".into(),
                    subject: "milestone".into(),
                    gate_path: gate.into(),
                    action: "pass".into(),
                    proof_kind: "fixture".into(),
                    proof_payload: json!({}),
                    git_tree_sha: "fixture".into(),
                    worker_id: "architect".into(),
                })
                .unwrap();
        }
        drop(store);
        let h = TaskHarness::attach(&workspace, &id);
        let stages: &[&str] = if seam {
            &["contract", "build", "review", "seam.integrate"]
        } else {
            &["contract", "build", "review", "deliver"]
        };
        for stage in stages {
            let worker = if *stage == "review" || *stage == "deliver" {
                "reviewer"
            } else {
                "builder"
            };
            let claim = TaskHarness::claim_at(workspace.root(), &id, stage, worker)
                .unwrap_or_else(|error| panic!("{stage}: {error:#}"));
            let task = h.store.inspect(&id).unwrap();
            let profile = h.store.profile_for_task(&task).unwrap();
            let gate = profile.by_stage(&task.stage).unwrap();
            let expected =
                contextunity_forge_mcp::core::tasks::gates::proof_template(gate, &profile).unwrap();
            assert_eq!(
                claim["workflow_guidance"]["expected_evidence_template"]["proof"], expected,
                "{stage}: claim proof template must match the resolved gate projection"
            );
            let mut proof =
                claim["workflow_guidance"]["expected_evidence_template"]["proof"].clone();
            if proof.get("contract_proof").is_some() {
                proof["contract_proof"]["seam_test_ref"] = json!(DEFAULT_SEAM);
            }
            if proof.get("command_proof").is_some() {
                proof["command_proof"]["command"] = json!("rust-check-and-test");
            }
            if proof.get("seam_test_proof").is_some() {
                proof["seam_test_proof"]["seam_test_refs"] = json!([DEFAULT_SEAM]);
            }
            let mut invalid = proof.clone();
            if invalid.get("contract_proof").is_some() {
                invalid["contract_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("seam_test_ref");
            } else if invalid.get("command_proof").is_some() {
                invalid["command_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("command");
            } else if invalid.get("review_proof").is_some() {
                invalid["review_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("decision");
            } else if invalid.get("seam_test_proof").is_some() {
                invalid["seam_test_proof"]
                    .as_object_mut()
                    .unwrap()
                    .remove("seam_test_refs");
            } else {
                invalid = json!({});
            }
            let error = h
                .submit_evidence(&TaskHarness::evidence_for_claim(&claim, invalid))
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("TASK_EVIDENCE_INVALID"),
                "{stage}: {error:#}"
            );
            if *stage == "seam.integrate" {
                let observed = tasks::verify_task(workspace.root(), &id, None).unwrap();
                assert!(observed["error"]
                    .as_str()
                    .unwrap()
                    .contains("TASK_AUTO_PROOF_UNAVAILABLE"));
                break;
            }
            let task = h.store.inspect(&id).unwrap();
            let mut evidence = h
                .evidence_for_gate(&task)
                .unwrap_or_else(|error| panic!("{stage}: {error:#}"));
            evidence.proof = proof;
            h.submit_evidence(&evidence)
                .unwrap_or_else(|error| panic!("{stage}: {error:#}"));
            if *stage == "contract" {
                workspace.write(".forge/runner_fixture_green", "");
            }
        }
    }
}

#[test]
fn claim_context_exposes_recursive_scheme_schema_from_custom_profile() {
    const PROFILE: &str = r#"
inherit: none
roles:
  builder: {}
  independent_reviewer: {}
gates:
  - id: contract
    role: builder
    proof: none
  - id: schema_input
    role: builder
    proof:
      scheme:
        type: object
        required: [report]
        additionalProperties: false
        properties:
          report:
            type: object
            required: [entries]
            additionalProperties: false
            properties:
              entries:
                type: array
                items:
                  type: object
                  required: [name]
                  additionalProperties: false
                  properties:
                    name:
                      type: string
                      enum: [accepted, rejected]
  - id: review
    role: independent_reviewer
    proof: review
    independent_from: schema_input
    reject_to: schema_input
  - id: deliver
    role: independent_reviewer
    proof: delivery
    receipt_review: review
    review_sources: [review]
    reject_to: schema_input
    auto_commit: false
"#;
    let workspace = TaskHarness::workspace();
    workspace.write("src/lib.rs", "pub fn schema_gate() {}\n");
    workspace.write(
        "forge-mcp.yaml",
        "tasks_db: .forge/tasks.sqlite\nacdd_profile: custom/profile.yaml\n",
    );
    workspace.write(".gitignore", CANONICAL_FORGE_GITIGNORE);
    workspace.write(
        "AGENTS.md",
        "# Task instructions\nLoad the contextunity-forge skill.\n",
    );
    workspace.write(
        ".agents/skills/contextunity-forge/SKILL.md",
        "# ContextUnity Forge\n",
    );
    workspace.write("custom/profile.yaml", PROFILE);
    let manifest = MilestoneFixtureBuilder::new("m-scheme")
        .status("active")
        .invariants(&["isolated"])
        .task("schema", &["src/"])
        .task_target("Deliver schema evidence")
        .task_proof_policy("seam-test-first")
        .build();
    workspace.write("docs/010-scheme.md", &manifest);
    let mut store = tasks::store(workspace.root()).unwrap();
    store
        .sync(
            &Milestone::parse(&manifest, "forge-mcp").unwrap(),
            "docs/010-scheme.md",
            workspace.root(),
        )
        .unwrap();
    let id = "forge-mcp/m-scheme:schema".to_owned();
    drop(store);
    let mut h = TaskHarness::attach(&workspace, &id);
    h.advance_to("schema_input").unwrap();
    let task_id = h.id.as_str();
    let claim = TaskHarness::claim_at(h.root(), task_id, "schema_input", "schema-author").unwrap();

    assert_eq!(
        claim["context_bundle"]["proof_schema"],
        json!({
            "type": "object",
            "properties": {
                "report": {
                    "type": "object",
                    "properties": {
                        "entries": {
                            "type": "array",
                            "properties": {},
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": {
                                        "type": "string",
                                        "properties": {},
                                        "required": [],
                                        "additionalProperties": null,
                                        "items": null,
                                "enum": ["accepted", "rejected"]
                                    }
                                },
                                "required": ["name"],
                                "additionalProperties": false,
                                "items": null,
                                "enum": []
                            },
                            "required": [],
                            "additionalProperties": null,
                            "enum": []
                        }
                    },
                    "required": ["entries"],
                    "additionalProperties": false,
                    "items": null,
                    "enum": []
                }
            },
            "required": ["report"],
            "additionalProperties": false,
            "items": null,
            "enum": []
        })
    );

    let properties = claim["context_bundle"]["proof_schema"]
        .pointer("/properties/report/properties/entries/items/additionalProperties")
        .expect("array item schema preserves the forbidden-property contract");
    assert_eq!(properties, false);

    let task = TaskHarness::persisted_descriptor(&h.store, task_id).unwrap();
    let before_state = || TaskHarness::task_validation_state(&h.store, task_id).unwrap();
    assert_eq!(task.stage, "schema_input");
    let unchanged = before_state();
    let invalid_proofs = [
        ("nested required field missing", json!({"report":{}})),
        (
            "wrong nested type",
            json!({"report":{"entries":"not-an-array"}}),
        ),
        (
            "enum value rejected",
            json!({"report":{"entries":[{"name":"unknown"}]}}),
        ),
        (
            "array element type rejected",
            json!({"report":{"entries":[42]}}),
        ),
        (
            "forbidden nested property rejected",
            json!({"report":{"entries":[],"extra":true}}),
        ),
    ];
    for (case, proof) in invalid_proofs {
        let mut invalid_evidence = TaskHarness::evidence_for_claim(&claim, json!({}));
        invalid_evidence.proof = json!({"scheme_proof":proof});
        let error = h.submit_evidence(&invalid_evidence).unwrap_err();
        assert!(
            error.to_string().contains("TASK_EVIDENCE_INVALID"),
            "{case} produced an unexpected error: {error:#}"
        );
        assert_eq!(
            before_state(),
            unchanged,
            "{case} mutated task/gate/claim state"
        );
    }

    let mut accepted_evidence = TaskHarness::evidence_for_claim(&claim, json!({}));
    accepted_evidence.proof =
        claim["workflow_guidance"]["expected_evidence_template"]["proof"].clone();
    assert_eq!(
        accepted_evidence.proof,
        json!({
            "scheme_proof":{"report":{"entries":[{"name":"accepted"}]}}
        })
    );
    let accepted = h.submit_evidence(&accepted_evidence).unwrap();
    assert_eq!(accepted["stage"], "review");
    let submitted = before_state();
    assert_eq!(submitted.status, "ready");
    assert_eq!(submitted.stage, "review");
    assert_eq!(submitted.submissions, unchanged.submissions + 1);
    let stored_gate = TaskHarness::latest_gate_state(&h.store, task_id, "schema_input").unwrap();
    assert_eq!(stored_gate.0, "passed");
    let stored_evidence: Value = serde_json::from_str(stored_gate.1.as_deref().unwrap()).unwrap();
    assert_eq!(
        stored_evidence["proof"]["scheme_proof"]["report"]["entries"][0]["name"],
        "accepted"
    );
}

#[test]
fn named_schema_claim_and_root_role_emit_equal_validator_accepted_proofs() {
    const PROFILE: &str = r#"
inherit: none
proofs:
  design:
    schema:
      type: object
      required: [summary]
      properties:
        summary: {type: string}
roles:
  builder: {}
  independent_reviewer: {}
gates:
  - id: contract
    role: builder
    proof: none
  - id: design_input
    role: builder
    proof: design
  - id: review
    role: independent_reviewer
    proof: review
    independent_from: design_input
    fail: design_input
  - id: deliver
    role: independent_reviewer
    proof: delivery
    receipt_review: review
    review_sources: [review]
    fail: design_input
    auto_commit: false
"#;
    let workspace = TaskHarness::workspace();
    workspace.write("src/lib.rs", "pub fn named_schema_gate() {}\n");
    workspace.write(
        "forge-mcp.yaml",
        "tasks_db: .forge/tasks.sqlite\nacdd_profile: custom/profile.yaml\n",
    );
    workspace.write(".gitignore", CANONICAL_FORGE_GITIGNORE);
    workspace.write("custom/profile.yaml", PROFILE);
    let manifest = MilestoneFixtureBuilder::new("m-named-schema")
        .status("active")
        .task("schema", &["src/"])
        .task_target("Provide the named schema evidence")
        .build();
    workspace.write("docs/010-named-schema.md", &manifest);
    let mut store = tasks::store(workspace.root()).unwrap();
    store
        .sync(
            &Milestone::parse(&manifest, "forge-mcp").unwrap(),
            "docs/010-named-schema.md",
            workspace.root(),
        )
        .unwrap();
    let task_id = "forge-mcp/m-named-schema:schema";
    drop(store);

    let mut harness = TaskHarness::attach(&workspace, task_id);
    harness.advance_to("design_input").unwrap();
    let claim =
        TaskHarness::claim_at(harness.root(), task_id, "design_input", "schema-author").unwrap();

    assert_eq!(claim["context_bundle"]["proof_schema"]["type"], "object");
    assert_eq!(
        claim["context_bundle"]["proof_schema"]["required"],
        json!(["summary"])
    );
    assert_eq!(claim["workflow_guidance"]["proof_kind"], "design");
    assert_eq!(
        claim["workflow_guidance"]["expected_evidence_template"]["proof"],
        json!({"scheme_proof": {"summary": "<value>"}})
    );

    // The runner receives the real root-role prompt and returns its projected proof.
    workspace.write(
        "capture-role.py",
        r#"import json, pathlib, sys
context = json.loads(sys.argv[1].split('\n\nGate context:\n', 1)[1])
proof = context['expected_response']['proof']
pathlib.Path('role-proof.json').write_text(json.dumps(proof))
print(json.dumps({'gate': context['gate'], 'session_id': context['session_id'], 'decision': 'pass', 'proof': proof}))
"#,
    );
    workspace.write(
        "docs/workflows/named.yaml",
        r#"version: 2
inherit: none
save: none
proofs:
  design:
    schema:
      type: object
      required: [summary]
      properties:
        summary: {type: string}
roles:
  builder:
    runner: "python3 capture-role.py '{prompt}'"
gates:
  - id: design_input
    role: builder
    proof: design
"#,
    );
    contextunity_forge_mcp::forge_tasks::engine::orchestrator::workflow::run_standalone_workflow(
        workspace.root(),
        std::path::Path::new("docs/workflows/named.yaml"),
        None,
    )
    .unwrap();
    let role_proof: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace.root().join("role-proof.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        claim["workflow_guidance"]["expected_evidence_template"]["proof"],
        role_proof
    );
}

#[test]
fn claim_and_root_role_agree_for_configured_proof_shapes() {
    let cases = [
        ("contract", "    proof: contract\n    command: fixture\n    expected_failure: {exit_codes: [101], stderr_contains: 'assertion failed'}\n", "", json!({"contract_proof":{"seam_test_ref":"<indexed test>","red_exit_code":101}})),
        ("command", "    proof: command\n    command: fixture\n", "", json!({"command_proof":{"command":"<registered command>","exit_code":0,"tests_passed":1,"tests_failed":0}})),
        ("seam", "    proof: seam_test\n    command: fixture\n", "", json!({"seam_test_proof":{"exit_code":0,"seam_test_refs":["<test reference>"],"tests_passed":1,"tests_failed":0}})),
        ("delivery", "    proof: delivery\n    review_sources: [review]\n", "", json!({"delivery_proof":{"delivered":true}})),
        ("delivery_closed", "    proof: delivery\n    review_sources: [review]\n", "proofs:\n  delivery:\n    schema:\n      type: object\n      required: [status]\n      additionalProperties: false\n      properties:\n        status: {type: string}\n", json!({"delivery_proof":{"status":"<value>"}})),
        ("delivery_additional_schema", "    proof: delivery\n    review_sources: [review]\n", "proofs:\n  delivery:\n    schema:\n      type: object\n      additionalProperties: {type: string}\n", json!({"delivery_proof":{}})),
        ("delivery_additional_matching", "    proof: delivery\n    review_sources: [review]\n", "proofs:\n  delivery:\n    schema:\n      type: object\n      additionalProperties: {type: boolean}\n", json!({"delivery_proof":{"delivered":true}})),
        ("delivery_const", "    proof: delivery\n    review_sources: [review]\n", "proofs:\n  delivery:\n    schema:\n      type: object\n      const: {status: done}\n", json!({"delivery_proof":{"status":"done"}})),
        ("delivery_enum", "    proof: delivery\n    review_sources: [review]\n", "proofs:\n  delivery:\n    schema:\n      type: object\n      enum: [{status: done}]\n", json!({"delivery_proof":{"status":"done"}})),
        ("none", "    proof: none\n", "", json!({"none_proof": {}})),
        ("absent", "", "", json!({"none_proof": {}})),
        (
            "inline",
            "    proof:\n      scheme:\n        type: object\n        required: [summary]\n        properties:\n          summary: {type: string}\n",
            "",
            json!({"scheme_proof": {"summary": "<value>"}}),
        ),
        (
            "review",
            "    proof: review\n    contours: guidance.contours\n",
            "",
            json!({"review_proof": {"decision": "pass", "contours": {"safety": {"applicable": true, "evidence": "<review evidence>"}}}}),
        ),
        (
            "override",
            "    proof: none\n",
            "proofs:\n  none:\n    schema:\n      type: object\n      required: [reason]\n      properties:\n        reason: {type: string}\n",
            json!({"none_proof": {"reason": "<value>"}}),
        ),
    ];
    let workspace = TaskHarness::workspace();
    workspace.write("src/lib.rs", "pub fn work() {}\n");
    workspace.write(".gitignore", CANONICAL_FORGE_GITIGNORE);
    for (name, proof_definition, catalog, expected) in cases {
        workspace.write(
            "forge-mcp.yaml",
            &format!("tasks_db: .forge/tasks-{name}.sqlite\nacdd_profile: custom/profile.yaml\n"),
        );
        let fixture = match name {
            "contract" => json!({
                "command": "printf 'assertion failed\\n' >&2; exit 101 # --seam-ref=tests/example.rs::test_case",
                "output": {"require_counts": false}
            }),
            "seam" => json!({
                "command": "printf 'test tests/example.rs::test_case ... ok\\ntest result: ok. 1 passed; 0 failed\\n'",
                "output": {"tests": {"passed": "^test (.+) \\.\\.\\. ok$"}}
            }),
            _ => json!("printf 'test result: ok. 1 passed; 0 failed\\n'"),
        };
        let common = format!("{catalog}commands:\n  fixture: {fixture}\nguidance:\n  contour.safety: Check safety\nroles:\n  builder:\n    runner: \"python3 capture-role.py '{{prompt}}'\"\n  independent_reviewer: {{}}\n");
        let task_profile = if name.starts_with("delivery") {
            format!("inherit: none\n{common}gates:\n  - id: contract\n    role: builder\n    proof: none\n  - id: review\n    role: independent_reviewer\n    proof: review\n    independent_from: contract\n    fail: contract\n  - id: deliver\n    role: independent_reviewer\n{proof_definition}    receipt_review: review\n    fail: contract\n    auto_commit: false\n")
        } else {
            format!("inherit: none\n{common}gates:\n  - id: contract\n    role: builder\n{proof_definition}  - id: review\n    role: independent_reviewer\n    proof: review\n    independent_from: contract\n    fail: contract\n  - id: deliver\n    role: independent_reviewer\n    proof: delivery\n    receipt_review: review\n    review_sources: [review]\n    fail: contract\n    auto_commit: false\n")
        };
        workspace.write("custom/profile.yaml", &task_profile);
        let manifest = MilestoneFixtureBuilder::new("m-parity")
            .status("active")
            .task("case", &["src/"])
            .task_target("Compare proof guidance")
            .build();
        workspace.write("docs/parity.md", &manifest);
        let mut store = tasks::store(workspace.root()).unwrap();
        store
            .sync(
                &Milestone::parse(&manifest, "forge-mcp").unwrap(),
                "docs/parity.md",
                workspace.root(),
            )
            .unwrap();
        drop(store);
        if name.starts_with("delivery") {
            let mut h = TaskHarness::attach(&workspace, "forge-mcp/m-parity:case");
            h.advance_to("review").unwrap();
            let review = TaskHarness::claim_at(
                workspace.root(),
                "forge-mcp/m-parity:case",
                "review",
                "reviewer",
            )
            .unwrap();
            let proof = review["workflow_guidance"]["expected_evidence_template"]["proof"].clone();
            h.submit_evidence(&TaskHarness::evidence_for_claim(&review, proof))
                .unwrap();
        }
        let claim = TaskHarness::claim_at(
            workspace.root(),
            "forge-mcp/m-parity:case",
            if name.starts_with("delivery") {
                "deliver"
            } else {
                "contract"
            },
            if name.starts_with("delivery") {
                "reviewer"
            } else {
                "author"
            },
        )
        .unwrap();
        let claim_proof = &claim["workflow_guidance"]["expected_evidence_template"]["proof"];
        assert_eq!(*claim_proof, expected, "{name}");
        workspace.write("capture-role.py", r#"import json, pathlib, sys
context = json.loads(sys.argv[1].split('\n\nGate context:\n', 1)[1])
proof = context['expected_response']['proof']
pathlib.Path('role-proof.json').write_text(json.dumps(proof))
if 'review_proof' in proof:
    proof['review_proof']['notes'] = 'Reviewed safety'
if 'contract_proof' in proof:
    proof['contract_proof']['seam_test_ref'] = 'tests/example.rs::test_case'
    proof['contract_proof']['red_exit_code'] = 101
if 'command_proof' in proof:
    proof['command_proof']['command'] = 'fixture'
if 'seam_test_proof' in proof:
    proof['seam_test_proof']['seam_test_refs'] = ['tests/example.rs::test_case']
print(json.dumps({'gate': context['gate'], 'session_id': context['session_id'], 'decision': 'pass', 'proof': proof}))
"#);
        let workflow = if name.starts_with("delivery") {
            format!("version: 2\ninherit: none\nsave: none\n{common}gates:\n  - id: review\n    role: builder\n    proof: review\n  - id: deliver\n    role: builder\n{proof_definition}")
        } else {
            format!("version: 2\ninherit: none\nsave: none\n{common}gates:\n  - id: contract\n    role: builder\n{proof_definition}")
        };
        workspace.write("docs/workflows/parity.yaml", &workflow);
        contextunity_forge_mcp::forge_tasks::engine::orchestrator::workflow::run_standalone_workflow(
            workspace.root(), std::path::Path::new("docs/workflows/parity.yaml"), None,
        ).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let role_proof: Value = serde_json::from_str(
            &std::fs::read_to_string(workspace.root().join("role-proof.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(*claim_proof, role_proof, "{name}");
    }
}

#[test]
fn inline_role_mode_adapts_guidance_text() {
    let root = acdd_workspace(DEFAULT_SEAM, "");
    root.write(
        "forge-mcp.yaml",
        "tasks_db: .forge/tasks.sqlite\nagents_guidance: docs/AGENTS.md\nacdd_profile: custom_profile.yaml\n",
    );
    root.write(
        "docs/AGENTS.md",
        "# Agent Guidance\nUse ContextUnity Forge.\n",
    );
    root.write("src/lib.rs", "pub fn work() -> bool { true }\n");

    let profile_yaml = r#"
inherit: none
roles:
  builder: {}
  contract_author: {}
  independent_reviewer:
    mode: inline
    reuse_on_reject: true
    guidance: "Spawn a dedicated reviewer subagent to review candidate diff across contours."
gates:
  - id: contract
    role: contract_author
    proof: none
  - id: build
    role: builder
    proof: none
  - id: review
    role: independent_reviewer
    proof: review
    reject_to: build
    independent_from: build
  - id: deliver
    role: independent_reviewer
    proof: delivery
    review_sources: [review]
    reject_to: build
"#;
    root.write("custom_profile.yaml", profile_yaml);

    let milestone_content = r#"---
id: m-inline-role
title: Inline role test
doc_type: contract
status: active
---
# Inline role test

```yaml
task_ref: inline-task
target: Verify inline role guidance
proof_policy: seam-test-first
scope:
  - src/lib.rs
```
"#;
    root.write("docs/milestones/010-inline.md", milestone_content);

    let sync_res = contextunity_forge_mcp::engine::tasks::manage(
        root.root(),
        serde_json::from_value(json!({
            "action": "sync",
            "milestone_ref": "docs/milestones/010-inline.md"
        }))
        .unwrap(),
    )
    .unwrap();
    let task_id = sync_res["tasks"][0]["task_id"].as_str().unwrap();

    let inspected = contextunity_forge_mcp::engine::tasks::manage(
        root.root(),
        serde_json::from_value(json!({
            "action": "inspect",
            "task_id": task_id
        }))
        .unwrap(),
    )
    .unwrap();
    // At contract stage, mode is subagent
    assert_eq!(inspected["workflow_guidance"]["mode"], "subagent");

    // Advance to review stage
    let mut h = TaskHarness::attach(&root, task_id);
    h.advance_to("review").unwrap();

    // Now claim review stage: mode must be inline and guidance must be adapted!
    let review_claim =
        TaskHarness::claim_at(h.root(), task_id, "review", "inline-reviewer").unwrap();

    let guidance = &review_claim["workflow_guidance"];
    assert_eq!(guidance["role"], "independent_reviewer");
    assert_eq!(guidance["mode"], "inline");
    let role_guide = guidance["role_guide"].as_str().unwrap();
    assert!(
        role_guide.contains(
            "Execute inline in the current agent to review candidate diff across contours."
        ),
        "expected adapted inline role guide, got: {role_guide}"
    );
    assert!(!role_guide.contains("Spawn a dedicated"));
}
