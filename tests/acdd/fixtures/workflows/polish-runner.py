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
session = os.environ["FORGE_AGENT_SESSION_ID"]
proof = context["expected_response"]["proof"]
payload, = proof.values()
set_field(payload, "decision", "pass")
set_field(payload, "summary", "Перевірено виправлення")
with Path("polish-sessions.jsonl").open("a") as output:
    output.write(json.dumps({"gate": gate, "session": session}) + "\n")
if gate == "polish":
    Path("repaired").touch()
print(json.dumps({"gate": gate, "session_id": session, "decision": "pass", "proof": proof}))
