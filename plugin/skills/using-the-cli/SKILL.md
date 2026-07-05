---
name: using-the-cli
description: 'Use when a tracedecay MCP call fails, times out, or the server is disconnected or unconfigured — every MCP tool is also a shell command, `tracedecay tool` plus the tool name. Switch to the CLI instead of querying .tracedecay databases directly or abandoning tracedecay.'
---

# Using the tracedecay CLI

The `tracedecay` binary exposes every MCP tool as a shell command. MCP and CLI hit the same project store and return the same payloads, so an MCP transport failure (timeout, disconnect, missing server config) loses nothing: run the same tool with the same arguments via `tracedecay tool <name>` and keep following whatever `tracedecay:*` skill you were in.

## The one rule for arguments

**The arguments of `tracedecay tool <name>` are the tool's MCP `arguments` object** — the same JSON you would send over MCP. Pass it whole with `--args`:

- **`--args -` + a quoted heredoc** — the canonical form. No shell escaping, no argv-length limit, byte-exact MCP parity. Use it whenever the payload has quotes, newlines, or any non-scalar value.
- **`--args '<json>'`** — inline JSON, only when the object is short and contains no single quotes.
- **`--args payload.json`** (or `--args @payload.json`) — read from a file; use for payloads near or over the ~128 KiB per-argument shell limit.

```bash
tracedecay tool multi_str_replace --args - <<'JSON'
{"path":"src/lib.rs","replacements":[["old, with 'quotes'","new $body"]]}
JSON
```

Nothing new to learn: you already know every tool's MCP schema, and `--args` carries that object verbatim. For quick scalar-only calls you may also spell top-level fields as `--key value` flags (`--key=value` works too); values are interpreted by the tool's schema, and anything that isn't a scalar must be JSON. See [references/tool-arg-catalog.md](references/tool-arg-catalog.md) for ready-to-copy examples of the hard-shape tools.

## Discovery

1. **List every tool → `tracedecay tool`** (no name): all tools grouped by category with one-line summaries.
2. **One tool's parameters → `tracedecay tool <name> --help`**: the tool's full description, a ready-to-copy usage line, each parameter with its type/required flag (enum values and array shapes included), and a generated `--args -` example for any tool whose payload is non-scalar.
3. **Everything else → `tracedecay --help`**: the non-tool subcommands (`init`, `sync`, `status`, `doctor`, `daemon`, `sessions`, `dashboard`, …) plus a quick-start trailer that restates this discovery flow. Every subcommand's own `--help` carries an `Examples:` section with real flag combinations and `Related:` cross-references — read it before improvising flags.

## Pre-flighting edits with `--dry-run`

Add `--dry-run` to parse, validate, and print the final arguments object as JSON **without dispatching the tool** — no daemon, no handler, no side effects. Use it to self-check a destructive edit-tool payload (`str_replace`, `multi_str_replace`, `replace_symbol`, `ast_grep_rewrite`, `insert_at`) before applying it, or to confirm you constructed the right object when a shape is tricky. Exits non-zero with the same corrective error as a real call if the arguments are wrong.

```bash
tracedecay tool multi_str_replace --args - --dry-run <<'JSON'
{"path":"src/lib.rs","replacements":[["old","new"]]}
JSON
```

## Reserved flags

- `--args <json|-|@file|file>` — whole MCP arguments object (see above). Mutually exclusive with `--key value` flags and positionals.
- `--dry-run` — validate + print the resolved arguments object, do not invoke the tool.
- `--json` — print the raw JSON result payload instead of the human-readable text rendering.
- `--project <path>` — pick the project root explicitly; otherwise the nearest initialised project walking up from cwd is used. (We use `--project`, not `-p`, because several tools have a `path` argument that filters files within the project.)
- `-h` / `--help` — print the tool's parameters and exit.
- Per-key values starting with `@` are read from that file (`--new-body @/tmp/body.txt`); `@-` reads stdin — handy for multi-line replacement bodies.

## Retrieving truncated responses

TraceDecay truncates large tool responses and emits a **handle** envelope
instead of the full body. The original text is cached in the active-project
store; dereference it rather than re-running the source tool — this works
identically over MCP (`tracedecay_retrieve`) and the CLI
(`tracedecay tool retrieve`).

- A prior response ended with a `handle` (e.g. `rh_…`) and the missing detail
  is actually needed → **dereference the handle → `tracedecay_retrieve`**
  (`handle` copied exactly). It returns the exact cached original text; it does
  not re-run the tool or re-read a file/session/node. Do not re-run the broad
  query, guess, or read a file again.
- You do NOT need the truncated tail → leave it; retrieval costs tokens.
- Handles are local, project-scoped, and expire; if `retrieve` reports an
  expired/unknown handle, re-run the original tool with a **narrower** query
  (see `tracedecay:using-tracedecay`) rather than retrying the stale handle. If
  the truncated response used a `project-id`/`project-path` selector, pass the
  same selector to `retrieve`.
- To open one session/summary node instead of a cached tool body → expand it
  with `tracedecay_lcm_expand`; `tracedecay:managing-session-context` drives the
  LCM store and past-session retrieval.

## When to switch

- An MCP call returns a client or transport error, times out, or the server drops mid-session.
- The tracedecay MCP server is not configured in this host but `tracedecay` is on `PATH`.
- A subagent or hook context has shell access but no MCP access.

After falling back, diagnose the MCP side with `tracedecay doctor` and `tracedecay tool runtime`, and tell the user the session is running on the CLI fallback (and why) instead of silently downgrading.

## Corrective errors are the feedback loop

If a call is rejected, the error message tells you exactly how to fix it — read it as the CLI's tab-completion:

- Unknown parameter → names the nearest valid parameter (`did you mean --limit?`) and lists all valid keys.
- Invalid enum → lists the allowed values verbatim (`not one of: complexity | lines | …`).
- Wrong type for a field → names the expected JSON type and shows the `--args -` heredoc form for non-scalars.
- Missing required → names the parameter and gives a one-line usage example.

Fix the call per the message and retry; do not abandon the CLI fallback for raw DB access or broad Grep.

## Guardrails

- Never query `.tracedecay/*.db` with sqlite3 or scripts — schemas are internal and change without notice. The CLI is the supported fallback, not raw DB access.
- Do not abandon tracedecay for broad Grep/file reads just because MCP transport failed; the CLI answers the same graph, memory, and session questions.
- CLI editing tools (`str_replace`, `replace_symbol`, …) mutate the working tree exactly like their MCP twins — apply the same care as `tracedecay:editing-safely`. Pre-flight with `--dry-run` when the payload is non-trivial.
- If the CLI also fails (binary missing or project not initialised), fall back to plain tools and suggest `tracedecay init` / `tracedecay doctor` to the user.

## If tools are deferred or MCP fails

- This skill *is* the MCP-failure path: run `tracedecay tool <name>` (see *The one rule for arguments* above) for any tool whose MCP call errored, timed out, or was never configured.
- Deferred (names listed without schemas) but MCP otherwise healthy: load once
  with ToolSearch — `select:tracedecay_retrieve,tracedecay_runtime` (add the
  tools the parent skill needs) — then call normally instead of shelling out.

## Deliverable

Do not end this workflow without: the same result the MCP tool would have
returned, plus a note that the CLI fallback was used and why. Report any
`tracedecay_metrics:` line to the user.
