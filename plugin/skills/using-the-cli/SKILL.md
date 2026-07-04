---
name: using-the-cli
description: 'Use when a tracedecay MCP call fails, times out, or the server is disconnected or unconfigured — every MCP tool is also a shell command, `tracedecay tool` plus the tool name. Switch to the CLI instead of querying .tracedecay databases directly or abandoning tracedecay.'
---

# Using the tracedecay CLI

The `tracedecay` binary exposes every MCP tool as a shell command. MCP and CLI hit the same project store and return the same payloads, so an MCP transport failure (timeout, disconnect, missing server config) loses nothing: run the same tool with the same arguments via `tracedecay tool <name>` and keep following whatever `tracedecay:*` skill you were in.

## Discovery

1. **List every tool → `tracedecay tool`** (no name): all tools grouped by category with one-line summaries.
2. **One tool's parameters → `tracedecay tool <name> --help`**: the tool's full description, a ready-to-copy usage line with its required flags, and each parameter with its type and required/optional flag.
3. **Everything else → `tracedecay --help`**: the non-tool subcommands (`init`, `sync`, `status`, `doctor`, `daemon`, `sessions`, `dashboard`, …) plus a quick-start trailer that restates this discovery flow. Every subcommand's own `--help` carries an `Examples:` section with real flag combinations and `Related:` cross-references — read it before improvising flags.

## Invocation

- Arguments are alternating `--key value` flags: `tracedecay tool search --query "parse config" --limit 10`.
- Tool names work with or without the `tracedecay_` prefix (`tool search` ≡ `tool tracedecay_search`).
- `--json` prints raw JSON; `--args '{"key":"value"}'` passes a whole JSON argument object; any value starting with `@` is read from that file (handy for multi-line replacement bodies, e.g. `--new-body @/tmp/body.txt`).
- `--project <path>` picks the project root explicitly; otherwise the nearest initialised project walking up from cwd is used.
- Truncated responses emit the same `handle` envelope as MCP — dereference with `tracedecay tool retrieve --handle rh_…` (see *Retrieving truncated responses* below).
- The required/optional flags for the common tools are catalogued in [references/tool-arg-catalog.md](references/tool-arg-catalog.md).

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

## Guardrails

- Never query `.tracedecay/*.db` with sqlite3 or scripts — schemas are internal and change without notice. The CLI is the supported fallback, not raw DB access.
- Do not abandon tracedecay for broad Grep/file reads just because MCP transport failed; the CLI answers the same graph, memory, and session questions.
- CLI editing tools (`str_replace`, `replace_symbol`, …) mutate the working tree exactly like their MCP twins — apply the same care as `tracedecay:editing-safely`.
- If the CLI also fails (binary missing or project not initialised), fall back to plain tools and suggest `tracedecay init` / `tracedecay doctor` to the user.

## If tools are deferred or MCP fails

- This skill *is* the MCP-failure path: run `tracedecay tool <name> --key value`
  for any tool whose MCP call errored, timed out, or was never configured.
- Deferred (names listed without schemas) but MCP otherwise healthy: load once
  with ToolSearch — `select:tracedecay_retrieve,tracedecay_runtime` (add the
  tools the parent skill needs) — then call normally instead of shelling out.

## Deliverable

Do not end this workflow without: the same result the MCP tool would have
returned, plus a note that the CLI fallback was used and why. Report any
`tracedecay_metrics:` line to the user.
