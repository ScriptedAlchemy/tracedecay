# Dashboard

The embedded dashboard is the daemon’s graphical client for code intelligence,
project memory, lossless context, work, configuration, diagnostics, and usage.
Start it with:

```bash
tracedecay dashboard --open
```

The dashboard uses generated Rust API contracts and the same application
operations as CLI, MCP, LSP, SDK, hooks, and host integrations. Browser code
never opens TraceDecay or host storage.

Every view displays the selected project/profile scope, evidence authority,
repository and generation provenance, coverage, and receipts. Warming, partial,
unavailable, denied, cancelled, retained, and recreation-required outcomes are
distinct UI states, never a blank successful panel.

Structure views present Grafeo-backed graph and vector results supplied by the
daemon. Conversation and memory views hydrate authorized content from its owning
authority. Work controls separate proposal, authorization, execution, and
repository effects. Diagnostics are read-only; maintenance actions are explicit
daemon operations with authorization and receipts.

The dashboard binds to loopback and is intended for the local operator. It
supports keyboard navigation, screen readers, responsive layouts, reduced
motion, bounded graph rendering, and a non-graph form for selected evidence.

## Hermes plugin and LCM diagnostics

Install Hermes through TraceDecay so the generated user plugin and its wrapper
configuration stay together:

```bash
tracedecay install --agent hermes
tracedecay doctor --agent hermes
```

The installer writes the plugin under `~/.hermes/plugins/tracedecay/` and
enables it through `plugins.enabled` in `~/.hermes/config.yaml`. Plugin-owned
settings live in the `plugins.tracedecay` configuration block. Hermes' own
`compression.enabled` setting is the global automatic-compaction switch; other
compression settings remain host configuration rather than TraceDecay storage
identity.

The wrapper invokes the current TraceDecay tool schema through
`tracedecay tool <name> --json --args <json>` and uses the host's real project
root or working directory for routing. The native context engine exposes
`lcm_grep`, `lcm_load_session`, `lcm_describe`, `lcm_expand`,
`lcm_expand_query`, `lcm_status`, and `lcm_doctor`; each maps to the matching
daemon-routed LCM operation.

The wrapper reads these runtime environment variables before its installed
defaults:

| Variable | Effect |
|---|---|
| `TRACEDECAY_DASHBOARD_URL` | Uses the specified existing dashboard server instead of spawning one. |
| `TRACEDECAY_BIN` | Selects the `tracedecay` executable used when the wrapper starts the dashboard. |
| `TRACEDECAY_DASHBOARD_PROJECT` | Selects the project root passed to the dashboard; when unset, the wrapper uses the Hermes process working directory. |

Hermes homes and profiles never select a TraceDecay project or store. The
wrapper's server is loopback-bound and inherits Hermes dashboard-session
protection; use the variables only for the wrapper's runtime route, not as a
store selector.

For a Hermes problem, run the doctor command first. For a session or
compression problem, inspect `lcm_status` and `lcm_doctor` (or the matching
`tracedecay_lcm_*` operation) and retain the reported coverage, retention,
payload, and provenance state. `unavailable`, `partial`, `denied`, and
`refresh_required` are diagnostic outcomes, not instructions to inspect a
database or rebuild it by hand. Authorized maintenance remains a separate
daemon operation with its own preview and receipt.
