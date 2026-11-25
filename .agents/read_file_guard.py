#!/usr/bin/env python3
"""Allow read_file only for paths outside the active repository workspace."""

import json
import os
import sys

DENY_MSG = (
    "read_file is blocked inside this repository: use acdd_read, acdd read, or ctx_read instead."
)


def deny(reason: str = DENY_MSG) -> None:
    print(json.dumps({"decision": "deny", "reason": reason}))


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        deny("read_file denied: invalid hook payload.")
        return

    args = (payload.get("toolCall") or {}).get("args") or {}
    raw_path = args.get("Path") or args.get("path") or args.get("file_path")
    if not isinstance(raw_path, str) or not raw_path.strip():
        deny()
        return

    if not os.path.isabs(raw_path):
        deny()
        return

    roots = payload.get("workspacePaths") or []
    if not roots and payload.get("cwd"):
        roots = [payload["cwd"]]

    real_target = os.path.realpath(raw_path)
    for root in roots:
        if not isinstance(root, str) or not root:
            continue
        real_root = os.path.realpath(root)
        try:
            if os.path.commonpath([real_target, real_root]) == real_root:
                deny()
                return
        except ValueError:
            continue

    print(json.dumps({"decision": "allow"}))


if __name__ == "__main__":
    main()
