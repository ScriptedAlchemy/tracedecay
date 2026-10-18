---
name: using-the-cli
description: Use when TraceDecay MCP tools are unavailable or failing, or a call must run from the shell with the same arguments and scope. Skip when the daemon itself is down, since the CLI needs it too. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Using the CLI

`tracedecay tool <name>` adapts the same operation and argument schema as MCP.
Read live `--help` for the installed version. CLI fallback helps a broken MCP
transport only while the daemon is available; it is not a reason to start,
restart, or bypass an unavailable or intentionally held daemon.

Pass complex arguments as one object through `--args -` with a quoted heredoc,
or a payload file, to preserve quotes, newlines, arrays, and exact identifiers.
Do not interpolate untrusted text into shell syntax. Scalar flags are convenient
only when their interpretation matches the live schema.

Preserve project/profile selectors and generation-bound handles across transport
changes. A truncated response's handle belongs to its issuing scope; retrieve
only the needed continuation. A stale or mismatched handle requires resolving
that original scope, not silently rerunning against the active project.

Argument validation is not mutation authorization. Distinguish CLI parsing
controls from an operation's own preview/dry-run fields using current help.
Apply and rollback consume returned identities and expected state unchanged.

Correct deterministic argument errors once. Preserve typed availability failures
and use bounded native evidence when appropriate; private database queries are
not an alternate public transport.

**Report:** the command that ran, its result, and why the CLI replaced MCP. Do
not start or restart the daemon to make the CLI work.
