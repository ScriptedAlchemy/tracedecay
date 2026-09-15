# TraceDecay and lean-ctx

TraceDecay and lean-ctx both try to reduce the amount of context an AI coding
agent has to process, but they center different product journeys. This is a
current-state comparison, not an import list, roadmap, or a statement that one
product replaces the other. Verify lean-ctx behavior in its current
[documentation](https://github.com/yvgude/lean-ctx) and TraceDecay behavior
through its daemon and product contract.

## Product focus

| Concern | TraceDecay | lean-ctx |
|---|---|---|
| Code and evidence | A daemon-owned authority serves code, project-memory, and session evidence with exact provenance, generation, coverage, and receipts. | Describes a local context layer for read modes, shell output, request-context compression, and context-use visibility. |
| Graph and durable state | Grafeo owns admitted graph/vector data; SQLite owns relational/content records. Clients do not open either store directly. | Describes a property graph and portable context packages alongside its context-management features. |
| Session context | Lossless Context Memory retains raw-message evidence and summary nodes, with bounded replay and drill-down through daemon operations. | Describes persistent session memory and recovery across chats. |
| Workflow and multi-agent work | The V2 product includes the daemon-owned work graph and typed workflow runtime: task evidence, ownership, authorization, execution, cancellation, recovery, collaboration, and receipts are product data, not host scripts. | Describes agent handoff and shared-context features. |
| Operating model | Hosts submit bounded hints and typed operations; daemon convergence, freshness, and maintenance remain explicit. | Describes agent wrappers, shell hooks, and an optional request proxy that can mediate context on the read and model-request paths. |

## Choosing a boundary

Use TraceDecay when the job requires attributable code, memory, session, task,
or workflow evidence through one daemon authority and explicit state semantics.
Use lean-ctx when its documented context-compression or mediation journey is the
one you need. The products may address different parts of an agent workflow;
neither comparison should be read as authorization to bypass its storage,
policy, or host-integration boundaries.

For TraceDecay, inspect the installed product rather than assuming freshness or
availability:

```bash
tracedecay status --json
tracedecay doctor
tracedecay tool
```

`status` reports selected authority and generation. `doctor` is read-only and
does not synchronize, repair, or recreate a store. The [V2 product contract](plans/tracedecay-v2/00-plan-set-index.md),
[work graph](plans/tracedecay-v2/24-canonical-task-plan-graph-and-multi-agent-executor.md),
and [workflow runtime](plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md)
define the supported TraceDecay boundaries.
