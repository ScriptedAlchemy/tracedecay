#!/usr/bin/env python3
"""Generate the deterministic 10x V2 synthetic benchmark corpus."""

import argparse
import hashlib
import json
from pathlib import Path


def canonical_json(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    fixture_root = Path(__file__).resolve().parents[1] / "fixtures" / "v2"
    manifest = json.loads((fixture_root / "manifest.json").read_text(encoding="utf-8"))
    scale = manifest["benchmark"]["scale_factor"]
    output = args.output / manifest["benchmark"]["output"]
    output.parent.mkdir(parents=True, exist_ok=True)

    rows = []
    for fixture in manifest["files"]:
        document = json.loads((fixture_root / fixture["path"]).read_text(encoding="utf-8"))
        for replica in range(scale):
            for record in document["records"]:
                row = {
                    "fixture_sha256": fixture["sha256"],
                    "provider_family": fixture["provider_family"],
                    "replica": replica,
                    "record": record,
                    "synthetic_id": hashlib.sha256(
                        f"{fixture['path']}:{replica}:{record['id']}".encode()
                    ).hexdigest()[:24],
                }
                rows.append(canonical_json(row))

    output.write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")
    receipt = {
        "records": len(rows),
        "scale_factor": scale,
        "sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
    }
    (args.output / "receipt.json").write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
    )


if __name__ == "__main__":
    main()
