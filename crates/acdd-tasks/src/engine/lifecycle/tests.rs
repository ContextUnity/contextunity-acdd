use super::proof_fanout::*;
use super::*;

#[cfg(test)]
mod inherited_proof_response_tests {
    use super::*;

    #[test]
    fn response_size_bound_preserves_inherited_task_proof_slices() {
        let inherited = json!([{
            "gate_path": "blueprint_synthesis.synthesize",
            "proof_kind": "blueprint",
            "proof": {"tasks": {"contract": {"goal": "preserved"}}}
        }]);
        let seam_partners = json!(["project/milestone:partner"]);
        let response = bound_task_response(json!({
            "task_id": "project/milestone:contract",
            "context_bundle": {
                "inherited_proofs": inherited,
                "seam_partners": seam_partners,
                "scope_symbols": ["x".repeat(acdd_core::response::MAX_OUTPUT_BYTES)],
            }
        }));

        assert_eq!(
            response.pointer("/context_bundle/inherited_proofs"),
            Some(&inherited),
            "the inherited task proof slice is authority context and must survive response bounding"
        );
        assert_eq!(
            response.pointer("/context_bundle/seam_partners"),
            Some(&seam_partners),
            "seam_partners must survive response bounding"
        );
        assert_eq!(response["byte_limited"], true);
    }
}

#[cfg(test)]
mod task_proof_fanout_validation_tests {
    use super::*;
    use crate::tasks::{
        ContractStatus, ContractType, MilestoneContractEntry, MilestoneContractMatrix, TaskSpec,
    };

    fn task(task_ref: &str, depends_on: &[&str]) -> Task {
        Task {
            subject_kind: crate::store::ExecutionSubjectKind::Milestone,
            task_id: format!("project/milestone:{task_ref}"),
            milestone_ref: "milestone".into(),
            spec: TaskSpec {
                task_ref: task_ref.into(),
                target: format!("implement {task_ref}"),
                spec: None,
                acceptance_criteria: Vec::new(),
                agent_type: None,
                acdd_profile: None,
                proof_policy: "strict".into(),
                scope: Vec::new(),
                extended_scope: Vec::new(),
                symbols: Vec::new(),
                extended_symbols: Vec::new(),
                scope_roots: Vec::new(),
                contract_revision: 1,
                depends_on: depends_on
                    .iter()
                    .map(|task_ref| (*task_ref).into())
                    .collect(),
                concurrency: None,
                invariants: Vec::new(),
                status: None,
                receipt: None,
                subtasks: Vec::new(),
            },
            digest: "digest".into(),
            contract_revision: 1,
            claim_revision: 0,
            status: "ready".into(),
            stage: "contract".into(),
            wave_index: 0,
            parked_worker_id: None,
            parked_worktree: None,
            blocked_by: None,
            profile_pin: None,
            authority_snapshot: None,
            initial_scope: Vec::new(),
            gate: 0,
            worker_id: None,
            worktree: None,
            completed_at: None,
            receipt: None,
            applicable_invariants: Vec::new(),
        }
    }

    fn contract_matrix() -> MilestoneContractMatrix {
        MilestoneContractMatrix {
            milestone_id: "repository/project/milestone".into(),
            entries: vec![MilestoneContractEntry {
                milestone_id: "repository/project/milestone".into(),
                source_task: "provider".into(),
                target_task: "consumer".into(),
                contract_type: ContractType::SeamDependency,
                contract_ref: "public-api".into(),
                revision: 1,
                status: ContractStatus::Synthesized,
            }],
        }
    }

    fn valid_proof() -> Value {
        json!({
            "cited_invariants": ["INV-TEST"],
            "cited_symbols": ["fn:public_api"],
            "global_negative_boundaries": ["do not change storage format"],
            "blackboard_decision_id": "decision-1",
            "tasks": {
                "provider": {
                    "goal": "define the API",
                    "outgoing_seams": [{
                        "target_task": "consumer",
                        "contract_type": "seam_dependency",
                        "contract_ref": "public-api"
                    }]
                },
                "consumer": {
                    "goal": "use the API",
                    "incoming_seams": [{
                        "source_task": "provider",
                        "contract_type": "seam_dependency",
                        "contract_ref": "public-api"
                    }]
                }
            }
        })
    }

    #[test]
    fn task_proof_fanout_uses_the_namespace_qualified_milestone_id() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("acdd.yaml"),
            "roots: [.]\ntasks_db: .acdd/tasks.sqlite\ntask_repository: repository\ntask_project: project\n",
        )?;
        let registry = Registry::load(dir.path())?;
        let workspace = &registry.selected(None)?[0];
        let mut contract = task("contract", &[]);
        contract.task_id = "project/milestone-42:contract".into();
        assert_eq!(
            milestone_identity_for_task(workspace, &contract)?,
            "repository/project/milestone-42"
        );
        Ok(())
    }

    #[test]
    fn task_proof_fanout_acceptance_matrix() -> Result<()> {
        let tasks = vec![task("provider", &[]), task("consumer", &["provider"])];
        let matrix = contract_matrix();
        let proof = valid_proof();
        for payload in [
            proof.clone(),
            json!({"evidence": {"proof": {"scheme_proof": proof}}}),
        ] {
            let proof = root_proof_with_task_map(&payload)?
                .context("typed scheme proof task map was not found")?;
            let slices = task_proof_slices(proof, &tasks, &matrix)?;
            let provider = slices
                .iter()
                .find(|(id, _)| id.ends_with(":provider"))
                .context("provider slice missing")?;
            let consumer = slices
                .iter()
                .find(|(id, _)| id.ends_with(":consumer"))
                .context("consumer slice missing")?;
            assert_eq!(slices.len(), 2);
            assert_eq!(consumer.1["goal"], "use the API");
            assert_eq!(consumer.1["cited_invariants"], json!(["INV-TEST"]));
            assert_eq!(consumer.1["blackboard_decision_id"], "decision-1");
            assert_eq!(provider.1["cited_symbols"], json!(["fn:public_api"]));
        }
        Ok(())
    }

    #[test]
    fn task_proof_fanout_rejection_matrix() {
        let base_proof = valid_proof();
        let base_tasks = vec![task("provider", &[]), task("consumer", &["provider"])];
        let matrix = contract_matrix();
        let mut cancelled = task("cancelled", &[]);
        cancelled.status = "cancelled".into();
        let cases = [
            (
                "missing slice",
                "/tasks/consumer",
                Value::Null,
                None,
                "missing non-cancelled task_ref 'consumer'",
            ),
            (
                "missing dependency seam",
                "/tasks/provider/outgoing_seams",
                json!([]),
                None,
                "dependency seam 'provider -> consumer'",
            ),
            (
                "unknown partner",
                "/tasks/consumer/incoming_seams",
                json!([{
                    "source_task": "missing", "contract_type": "seam_dependency", "contract_ref": "public-api"
                }]),
                None,
                "unknown or cancelled task_ref",
            ),
            (
                "cancelled partner",
                "/tasks/consumer/incoming_seams",
                json!([{
                    "source_task": "cancelled", "contract_type": "seam_dependency", "contract_ref": "public-api"
                }]),
                Some(cancelled.clone()),
                "unknown or cancelled task_ref",
            ),
            (
                "cancelled declarer",
                "/tasks/cancelled",
                json!({
                    "outgoing_seams": [{"target_task": "consumer"}]
                }),
                Some(cancelled),
                "cancelled task_ref 'cancelled' cannot declare seam edges",
            ),
            (
                "unadmitted incoming",
                "/tasks/consumer/incoming_seams",
                json!([{
                    "source_task": "other", "contract_type": "shared_helper", "contract_ref": "other-helper"
                }]),
                Some(task("other", &[])),
                "incoming_seams edge 'other -> consumer'",
            ),
            (
                "unadmitted outgoing",
                "/tasks/provider/outgoing_seams",
                json!([{
                    "target_task": "other", "contract_type": "shared_helper", "contract_ref": "other-helper"
                }]),
                Some(task("other", &[])),
                "outgoing_seams edge 'provider -> other'",
            ),
        ];
        for (label, pointer, value, extra_task, expected) in cases {
            let mut proof = base_proof.clone();
            let mut tasks = base_tasks.clone();
            if let Some(extra) = extra_task {
                proof["tasks"][&extra.spec.task_ref] = json!({"goal": "unrelated task"});
                tasks.push(extra);
            }
            if value.is_null() {
                proof["tasks"].as_object_mut().unwrap().remove("consumer");
            } else {
                *proof.pointer_mut(pointer).unwrap() = value;
            }
            let error = task_proof_slices(&proof, &tasks, &matrix).unwrap_err();
            assert!(error.to_string().contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn bound_task_response_for_root_rejects_oversized_protected_fields() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("acdd.yaml"),
            "roots: [src]\ndocs: [docs]\nresponse:\n  max_output_bytes: 8192\n",
        )?;
        let oversized = json!({
            "status": "ready",
            "stage": "polish",
            "context_bundle": {
                "inherited_proofs": [{
                    "proof": "x".repeat(6000),
                }]
            }
        });
        let error = bound_task_response_for_root(dir.path(), oversized)
            .unwrap_err()
            .to_string();
        assert!(error.contains("TASK_RESPONSE_OVERFLOW"), "{error}");
        Ok(())
    }
}
