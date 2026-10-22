#!/usr/bin/env python3
from __future__ import annotations

import json
import sys

try:
    output = json.load(sys.stdin)
except json.JSONDecodeError as error:
    print(f"npm pack returned invalid JSON: {error}", file=sys.stderr)
    raise SystemExit(1) from error

if not isinstance(output, dict) or len(output) != 1:
    print("npm pack must return one package-name-keyed record", file=sys.stderr)
    raise SystemExit(1)

metadata = next(iter(output.values()))
filename = metadata.get("filename") if isinstance(metadata, dict) else None
if not isinstance(filename, str) or not filename:
    print("npm pack record must contain a filename", file=sys.stderr)
    raise SystemExit(1)

print(filename)
