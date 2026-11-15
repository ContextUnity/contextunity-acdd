import json
import os
from pathlib import Path
import sys

context = json.loads(sys.argv[1].split("\n\nGate context:\n", 1)[1])
gate = context["gate"]
proof = context["expected_response"]["proof"]
payload, = proof.values()
decision = "pass"
if gate == "prerequisite":
    payload.update(prerequisite="prerequisite")
elif gate == "synthesize":
    if Path("synthesized").exists():
        payload.update(plan_slug="new-plan", artifact_path="new/path")
    else:
        Path("synthesized").touch()
        payload.update(plan_slug="old-plan", artifact_path="old/path", removed="stale")
elif gate == "review":
    if not Path("reviewed").exists():
        Path("reviewed").touch()
        decision = "reject"
    payload["decision"] = decision
print(json.dumps({"gate": gate, "session_id": os.environ["ACDD_AGENT_SESSION_ID"],
                  "decision": decision, "proof": proof,
                  "findings": {"issue": "repair"} if decision == "reject" else None}))
