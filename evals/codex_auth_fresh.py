#!/usr/bin/env python3
"""Exit non-zero unless a Codex auth.json can be copied without refreshing.

A ChatGPT login rotates its refresh token on every refresh. If an eval's copy
of auth.json refreshes, the real ~/.codex/auth.json is left holding a spent
refresh token and the user's Codex CLI is signed out. Codex refreshes when the
access token nears expiry or the last refresh is about 8 days old, so evals
only copy a login that will not hit either limit during the run.
"""

import base64
import json
import sys
import time
from datetime import datetime

MARGIN_S = 48 * 3600
MAX_REFRESH_AGE_S = 7 * 86400


def main(path: str) -> int:
    auth = json.load(open(path))
    if auth.get("OPENAI_API_KEY"):
        return 0
    tokens = auth.get("tokens") or {}
    payload = tokens.get("access_token", "").split(".")[1:2]
    if not payload:
        print(f"{path}: no ChatGPT access token", file=sys.stderr)
        return 1
    claims = json.loads(base64.urlsafe_b64decode(payload[0] + "=" * (-len(payload[0]) % 4)))
    now = time.time()
    left = claims.get("exp", 0) - now
    last = auth.get("last_refresh")
    age = now - datetime.fromisoformat(last[:26].rstrip("Z") + "+00:00").timestamp() if last else MAX_REFRESH_AGE_S + 1
    if left < MARGIN_S or age > MAX_REFRESH_AGE_S:
        print(
            f"{path}: Codex login expires in {left / 3600:.0f} h (last refresh {age / 86400:.1f} days ago).\n"
            "An eval copy would refresh it and sign the real CLI out. Run `codex login` (or any real\n"
            "`codex` command if the login is still valid) to refresh it in place, then rerun.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
