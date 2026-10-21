#!/usr/bin/env python3
"""Select runner inputs and hash the same scenario bytes used for each prompt."""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path


def main() -> None:
    directory, selected, include_deferred, workflow, include_baseline = sys.argv[1:]
    selected_ids = selected.split()
    for path in sorted(Path(directory).glob("*.json")):
        source = path.read_bytes()
        scenario = json.loads(source)
        if workflow:
            if (scenario.get("learned_skill") or {}).get("workflow") != workflow:
                continue
        elif (scenario.get("status") == "learned"
              or (scenario.get("status") == "deferred" and include_deferred != "1")
              or (scenario.get("status") == "baseline" and include_baseline != "1")):
            continue
        if selected_ids and scenario["id"] not in selected_ids:
            continue
        outcome = scenario.get("outcome", {})
        effect = "1" if outcome.get("files") or outcome.get("behavior") else "0"
        digest = hashlib.sha256(source).hexdigest()
        prompt = scenario["prompt"].replace("\t", " ").replace("\n", " ").replace("\r", " ")
        for host in scenario.get("hosts", []):
            print("\t".join([scenario["id"], host, scenario.get("fixture", "main"),
                             effect, digest, prompt]))


if __name__ == "__main__":
    main()
