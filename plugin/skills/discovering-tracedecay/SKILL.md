---
name: discovering-tracedecay
description: Use when you know the task but not which TraceDecay tool or operation covers it, including memory and administration tasks. Skip when routing-tracedecay already names a call for the question. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Discovering TraceDecay

Every TraceDecay tool is on tools/list. Tools outside the most-used set may be
stubs with a short description and an open schema: call a stub by name, and a
rejected call returns its full input schema. `tracedecay tool <name> --help`
shows the same parameters. This is capability discovery, not a prerequisite for
ordinary reads, local edits, or clarification.

Choose by the missing evidence: exploration locates code; tracing follows call
relationships; impact connects changes to dependents and tests; review evaluates
a diff; editing handles structural mutation. Durable facts and raw session
history have separate stores and retrieval workflows. When the moment is clear
and the tool is not, read `routing-tracedecay` and make the one call it names.

MCP and the generic CLI adapt the same daemon operations. A failed MCP transport
does not establish daemon failure; see `using-the-cli` when transport matters.

For project and multi-root identity, configuration changes, Context Scout
controls, or storage recovery, read
[administration authority](references/administration-authority.md).

**Report:** the skill or operation that covers the task and the one call to make
next. Do not list the catalog.
