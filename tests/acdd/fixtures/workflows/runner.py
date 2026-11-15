import json
import os
from pathlib import Path
import sys

context = json.loads(sys.argv[1].split("\n\nGate context:\n", 1)[1])
gate = context["gate"]
session = os.environ["ACDD_AGENT_SESSION_ID"]
with Path("workflow-role-sessions.jsonl").open("a") as output:
    output.write(json.dumps({"gate": gate, "goal": context["goal"], "session": session, "findings": context["findings"], "config": os.environ.get("ACDD_CONFIG"), "profile": os.environ.get("ACDD_PROFILE"), "db": os.environ.get("ACDD_DB")}) + "\n")
if Path("missing-decision").exists():
    sys.exit(0)
decision = "pass"
findings = None
proof = context["expected_response"]["proof"]
payload, = proof.values()
if Path("root-proof.json").exists():
    payload.update(json.loads(Path("root-proof.json").read_text()))
if not isinstance(payload, dict):
    raise ValueError("invalid proof template")
if "decision" in payload:
    if not isinstance(payload["decision"], str):
        raise ValueError("proof decision must be a string")
    if "summary" in payload and not isinstance(payload["summary"], str):
        raise ValueError("proof summary must be a string")
    if not Path("review-rejected").exists():
        Path("review-rejected").touch()
        decision = "reject"
        findings = {"scope_findings": ["Apply the required correction"]}
    payload["decision"] = decision
    if "summary" in payload:
        payload["summary"] = "Inspected this workflow run"
response = {"gate": gate, "session_id": session, "decision": decision, "proof": proof, "findings": findings}
mode = Path("role-mode").read_text().strip() if Path("role-mode").exists() else ""
if "decision" in payload:
    if mode in ("embedded", "fenced-embedded", "empty", "conflict") and decision == "reject":
        payload["step_findings"] = [] if mode == "empty" else ["Repair this step"]
        response["findings"] = {"step_findings": ["Different"]} if mode == "conflict" else None
    if mode in ("nested-match", "nested-conflict", "nested-sibling-match", "nested-sibling-conflict") and decision == "reject":
        payload["findings"] = {"step_findings": ["Repair this step"]}
        if mode.startswith("nested-sibling"):
            payload["step_findings"] = ["Different" if mode.endswith("conflict") else "Repair this step"]
            response["findings"] = None
        else:
            response["findings"] = {"step_findings": ["Different" if mode.endswith("conflict") else "Repair this step"]}
    if mode == "missing":
        response["findings"] = None
    if mode == "mismatch":
        payload["decision"] = "reject" if decision == "pass" else "pass"
    if mode == "identity":
        response["session_id"] = "wrong"
if mode in ("array", "fenced-array"):
    response = [gate, session, decision, proof, None, None, None]
text = json.dumps(response)
if mode in ("duplicate", "fenced-duplicate"):
    text = text.replace('"decision": "pass"', '"decision": "reject", "decision": "pass"', 1)
if mode in ("fenced", "fenced-embedded", "fenced-array", "fenced-duplicate"):
    text = "```json\n" + text + "\n```"
elif mode == "incomplete":
    text = "```json\n" + text
elif mode == "trailing":
    text += " trailing prose"
elif mode == "multiple":
    text += "\n" + text
print(text)
