#!/usr/bin/env python3
"""Exit after shell completion while leaving a stubborn descendant."""

from __future__ import annotations

import json
import subprocess
import sys


subprocess.Popen(
    [
        sys.executable,
        "-c",
        (
            "import signal,time;"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN);"
            "time.sleep(30)"
        ),
    ],
    stdin=subprocess.DEVNULL,
)
print(
    json.dumps(
        {
            "availability": "unavailable",
            "error": "daemon unavailable after shell completion",
        },
        separators=(",", ":"),
        sort_keys=True,
    ),
    flush=True,
)
raise SystemExit(72)
