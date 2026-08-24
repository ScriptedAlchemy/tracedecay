---
name: discovering-tracedecay
description: 'Use when asking what TraceDecay can do, which TraceDecay tool, skill, command, or workflow fits a task, or how to discover capabilities when MCP is absent or optional.'
---

# Discovering TraceDecay

Treat skills as workflow guidance and the CLI or MCP as interchangeable tool
transports. MCP is optional: capability discovery must work from a shell alone.

## Discovery ladder

1. Run **`tracedecay tool`** to list every tool by category with a one-line
   summary. Scan the relevant category; do not guess tool names.
2. Run **`tracedecay tool <name> --help`** for the selected tool's exact
   arguments, types, enum values, and examples.
3. Run **`tracedecay --help`** for lifecycle commands such as `init`, `sync`,
   `doctor`, `daemon`, `sessions`, `analytics`, `automation`, and `upgrade`.
   Then inspect one command with `tracedecay <command> --help`.
4. Choose the smallest matching workflow and continue with its skill:

| Intent | Workflow skill |
|---|---|
| Locate or understand code | `tracedecay:exploring-code` |
| Trace calls or assess blast radius | `tracedecay:tracing-functions` / `tracedecay:assessing-impact` |
| Review or edit code | `tracedecay:reviewing-changes` / `tracedecay:editing-safely` |
| Recover decisions or session history | `tracedecay:project-memory` / `tracedecay:managing-session-context` |
| Diagnose adoption or host health | `tracedecay:diagnosing-analytics` / `tracedecay doctor` |
| Fix compiler or type errors | `tracedecay:fixing-build-and-type-errors` |

## Rules

- Prefer live `--help` output over remembered flags; the installed binary is
  the source of truth.
- Do not query TraceDecay databases directly to discover capabilities.
- Do not dump the entire catalog into the answer. Name the selected workflow,
  exact command or tool, and why it matches.
- Once a tool is selected, use MCP when available or the identical
  `tracedecay tool <name>` CLI surface when it is not. See
  `tracedecay:using-the-cli` for argument construction.

## Deliverable

Return the chosen skill or workflow, the exact next command/tool, and one-line
reason. If no capability fits, say what is missing instead of inventing one.
