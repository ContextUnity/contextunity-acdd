import json
import os
from pathlib import Path
import sys


def set_field(payload, key, value):
    if key not in payload:
        sys.exit(f"missing expected proof template key: {key}")
    if type(payload[key]) is not type(value):
        sys.exit(f"invalid expected proof template type: {key}")
    payload[key] = value


context = json.loads(sys.argv[1].split("\n\nGate context:\n", 1)[1])
gate = context["gate"]
session = os.environ["ACDD_AGENT_SESSION_ID"]
subject = context["subject"]
kind = next(name for name in ("milestone", "plan", "workflow") if "author-" + name in subject)
paths = {
    "milestone": "docs/milestones/IN-001-pryklad.md",
    "plan": "docs/plans/pryklad.md",
    "workflow": "docs/workflows/pryklad.workflow.yaml",
}
path = Path(paths[kind])
proof = context["expected_response"]["proof"]
payload, = proof.values()
decision = "pass"
findings = None
if gate == "synthesize":
    repaired = Path("rejected").exists()
    if kind == "milestone":
        fields = dict(milestone_id="IN-001-pryklad", manifest_path=str(path),
                      wave_dag=[["first"]], scopes_disjoint=True, protected_files_excluded=True)
    elif kind == "plan":
        fields = dict(plan_slug="pryklad", plan_path=str(path), step_count=2,
                      dependency_dag={"Другий": ["Перший"]}, scopes_valid=True)
    else:
        fields = dict(workflow_name="pryklad", workflow_path=str(path), gate_count=1,
                      transitions_valid=True, taskless=True)
    for key, value in fields.items():
        set_field(payload, key, value)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(Path("author-final" if repaired else "author-draft").read_text())
else:
    if not Path("rejected").exists():
        decision = "reject"
        field = {"milestone": "scope_findings", "plan": "step_findings",
                 "workflow": "structural_findings"}[kind]
        set_field(payload, field, ["Виправити чернетку"])
        findings = {field: payload[field]}
    set_field(payload, "decision", decision)
    set_field(payload, "summary", "Перевірено документ")
    if decision == "reject":
        Path("rejected").touch()
with Path("author-sessions.jsonl").open("a") as log:
    log.write(json.dumps({"gate": gate, "session": session, "decision": decision}) + "\n")
print(json.dumps({"gate": gate, "session_id": session, "decision": decision,
                  "proof": proof, "findings": findings}))
