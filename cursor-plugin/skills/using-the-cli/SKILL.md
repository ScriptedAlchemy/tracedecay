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
- Truncated responses emit the same `handle` envelope as MCP — dereference with `tracedecay tool retrieve --handle rh_…`.

## When to switch

- An MCP call returns a client or transport error, times out, or the server drops mid-session.
- The tracedecay MCP server is not configured in this host but `tracedecay` is on `PATH`.
- A subagent or hook context has shell access but no MCP access.

After falling back, diagnose the MCP side with `tracedecay doctor` and `tracedecay tool runtime`, and tell the user the session is running on the CLI fallback (and why) instead of silently downgrading.

## Guardrails

- Never query `.tracedecay/*.db` with sqlite3 or scripts — schemas are internal and change without notice. The CLI is the supported fallback, not raw DB access.
- Do not abandon tracedecay for broad Grep/file reads just because MCP transport failed; the CLI answers the same graph, memory, and session questions.
- CLI editing tools (`str_replace`, `replace_symbol`, …) mutate the working tree exactly like their MCP twins — apply the same care as `tracedecay:atomic-code-edits`.
- If the CLI also fails (binary missing or project not initialised), fall back to plain tools and suggest `tracedecay init` / `tracedecay doctor` to the user.

## Output

- The same result the MCP tool would have returned, plus a note that the CLI fallback was used and why.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
