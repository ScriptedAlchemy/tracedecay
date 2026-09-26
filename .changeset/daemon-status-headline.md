---
"tracedecay": patch
---

`tracedecay daemon status` now leads with the daemon's own state from its socket probe (`state: running` when it answers initialize) and reports the service manager on a separate `service manager:` line, so a shell that cannot reach the systemd user manager no longer reads a serving daemon as unavailable.
