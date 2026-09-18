#!/usr/bin/env python3
"""Exit after shell completion while leaving a stubborn descendant."""

from __future__ import annotations

import json
import subprocess
import sys


# The descendant must already be ignoring SIGTERM when this host exits;
# otherwise the harness's TERM lands during interpreter start-up and kills
# what was meant to be stubborn. It reports readiness after installing the
# handler, and this host waits for that line before exiting.
stubborn = subprocess.Popen(
    [
        sys.executable,
        "-c",
        (
            "import signal,sys,time;"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN);"
            "sys.stdout.write('ready\\n'); sys.stdout.flush();"
            "time.sleep(30)"
        ),
    ],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
)
assert stubborn.stdout is not None
if stubborn.stdout.readline() != b"ready\n":
    raise SystemExit("stubborn descendant did not report readiness")
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
