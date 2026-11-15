"""Protocol substitute for unavailable LLM roles; ACDD owns all state and checks."""
import json
from pathlib import Path
import sys

context = json.loads(sys.argv[1].split("\n\nGate context:\n", 1)[1])
root = Path.cwd()
with (root / ".acdd/wave-roles.jsonl").open("a") as log:
    log.write(json.dumps(context) + "\n")
if (root / ".acdd/no-wave-decision").exists():
    sys.exit(0)
proof = context["expected_response"]["proof"]
payload = next(iter(proof.values()), None)
response = {"gate": context["gate"], "session_id": context["session_id"], "decision": "pass", "proof": proof}
if context["gate"] == "blueprint_synthesis.review" and (root / ".acdd/stop-blueprint-review").exists():
    sys.exit(17)
if context["gate"] == "blueprint_synthesis.review" and not (root / ".acdd/blueprint-rejected").exists():
    (root / ".acdd/blueprint-rejected").touch()
    response["decision"] = "reject"
    response["findings"] = {"scope_findings": ["Revise the blueprint"]}
if isinstance(payload, dict) and (root / ".acdd/corrupt-blueprint-missing").exists() and "tasks" in payload:
    del payload["blackboard_decision_id"]
if isinstance(payload, dict) and (root / ".acdd/corrupt-blueprint-tasks").exists() and "tasks" in payload:
    payload["tasks"] = []
if isinstance(payload, dict) and (root / ".acdd/corrupt-blueprint-nested").exists() and "tasks" in payload:
    payload["tasks"] = {"first": {"layer": 7}}
def materialize(value, field="", placeholder_slice=False):
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            if field == "tasks" and key.startswith("<") and key.endswith(">"):
                if "first" not in value and not (root / ".acdd/invalid-task-proof-map").exists():
                    result["first"] = materialize(item, placeholder_slice=True)
            elif field == "tasks":
                result[key] = item
            else:
                result[key] = materialize(item, key, placeholder_slice)
        return result
    if isinstance(value, list):
        if placeholder_slice and field in ("incoming_seams", "outgoing_seams"):
            endpoint = "source_task" if field == "incoming_seams" else "target_task"
            example = {endpoint: "<value>", "contract_type": "seam_dependency",
                       "contract_ref": "<value>", "specification": "<value>"}
            value = [item for item in value if item != example]
        return [materialize(item, field, placeholder_slice) for item in value]
    if isinstance(value, str) and value.startswith("<") and value.endswith(">"):
        return {"decision": response["decision"], "blackboard_decision_id": "wave-blueprint", "layer": "fixture seam",
                "summary": "Exercise the pre-task proof boundary."}.get(field, "fixture value")
    return value

response["proof"] = materialize(proof)
if (root / "root-proof.json").exists():
    next(iter(response["proof"].values())).update(json.loads((root / "root-proof.json").read_text()))
if context["gate"] == "milestone_review":
    if (root / ".acdd/stop-wave-review").exists():
        sys.exit(17)
    response["binding"] = context["subject_context"]["binding"]
    response["integration_review"] = {
        "worker_id": context["session_id"], "decision": "pass",
        "summary": "Reviewed the supplied fixture authority, task receipts and implementation binding",
        "scope_findings": [], "contract_findings": [],
    }
    if (root / ".acdd/stale-wave-review").exists():
        response["binding"]["implementation_digest"] = "stale"
print(json.dumps(response))
