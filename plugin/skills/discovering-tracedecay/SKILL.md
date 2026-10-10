---
name: discovering-tracedecay
description: Find the supported TraceDecay operation for a code-intelligence, memory, or administration task.
---

# Discovering TraceDecay

Default `tracedecay serve` lists a core tool set plus `tracedecay_tool_search`.
Tools outside that list are not registered until you search. Call
`tracedecay_tool_search` with keywords or an exact tool name to load them into
tools/list. An empty query names every tool that is still unloaded. The Claude
Code plugin instead lists every tool and marks the core set
`_meta["anthropic/alwaysLoad"]`; use Claude's native ToolSearch for the rest.
Every catalog tool also answers a direct `tools/call` by name. Use live tool
descriptions and `tracedecay tool <name> --help` for arguments.

This is capability discovery, not a prerequisite for ordinary reads, local
edits, or clarification. When the moment is clear and the tool is not, read
`routing-tracedecay` and make the one call it names. If that name is missing
from tools/list, search for it first.

| Group | Search when | Examples |
| --- | --- | --- |
| Core (already listed) | source reads, grep, symbol search, context, callers, diff context, test mapping, files, session and fact recall | `tracedecay_grep`, `tracedecay_search`, `tracedecay_context` |
| Impact and blast radius | dependents, risk of a change | `tracedecay_impact`, `tracedecay_affected`, `tracedecay_affected_tests` |
| Call chains | callees and reverse edges | `tracedecay_callees` |
| Git, PR, and branch | review context beyond the working tree | `tracedecay_git_diff` |
| Code health | architecture, coupling, structural test risk | `tracedecay_health` |
| Refactors and edits | structural mutation | `tracedecay_str_replace`, `tracedecay_ast_grep_rewrite` |
| Tests | run a selected set | `tracedecay_run_affected_tests` |
| Workflows and work items | tracked work, attempts, definitions | `tracedecay_work_*`, `tracedecay_workflow_*` |
| Memory | durable facts beyond search | `tracedecay_fact_store_get` |
| Configuration | profile or project settings | `tracedecay_configuration_get` |
| Diagnostics | compiler or runtime evidence | `tracedecay_diagnostics`, `tracedecay_diagnose` |

Choose by the missing evidence: exploration locates code; tracing follows call
relationships; impact connects changes to dependents and tests; review evaluates
a diff; editing handles structural mutation. Durable facts and raw session
history have separate stores and retrieval workflows.

MCP and the generic CLI adapt the same daemon operations. A failed MCP transport
does not establish daemon failure; see `using-the-cli` when transport matters.

For project and multi-root identity, configuration changes, Context Scout
controls, or storage recovery, read
[administration authority](references/administration-authority.md).
