# TraceDecay Specialist Agents Design

## Goal

Expand TraceDecay's bundled agent set beyond generic code exploration, code
health, and session history. The new agents cover the major operational and
review surfaces without requiring MCP and without granting autonomous mutation
authority.

## Agent set

1. `runtime-storage-doctor`: diagnoses daemon, database, migration, project
   identity, move/rename/symlink, and index-health failures. It may run
   read-only doctor, diagnostics, status, and storage inspection commands. It
   reports root cause and a verification plan; the parent performs repairs.
2. `cross-host-integration-auditor`: checks install/update/configuration and
   capability parity across Codex, Claude, Cursor, and supported integrations.
   It verifies skill, command, agent, hook, and tool discovery from packaged
   artifacts and host-native diagnostics.
3. `change-risk-reviewer`: reviews a PR, branch, or diff using semantic change
   context, callers, affected tests, diagnostics, redundancy, and safety scans.
   It returns only evidence-backed findings and does not edit or merge.
4. `usage-intelligence-analyst`: analyzes tool adoption, fact recall and
   feedback, hint relevance, session evidence, and discovery-channel gaps. It
   uses TraceDecay analytics and transcript tools rather than raw databases.
5. `automation-auditor`: inspects automation cycles, managed-skill drafts,
   validation artifacts, apply policy, retry behavior, and adoption outcomes.
   It does not approve, install, disable, archive, or apply artifacts.

The existing `code-explorer`, `code-health-auditor`, and `session-historian`
remain. The final bundled set is eight agents.

## Transport and discovery

Every agent prompt names intent, allowed evidence sources, stopping conditions,
and a concise result schema. MCP is an optional transport. When MCP is absent,
the agent uses the equivalent `tracedecay tool <name>` CLI surface and normal
top-level CLI commands. It must discover exact flags from live `--help`, never
query `.tracedecay` databases directly, and never invent a missing tool.

The shared source lives under `plugin/agents/`. Claude and Cursor receive their
native agent files. Codex materializes equivalent model-invocable agent skills
through the existing bundle generator. Names, descriptions, bodies, and
capability boundaries stay source-derived and parity-tested.

## Authority boundaries

All eight bundled agents are read-only analysts. They may inspect local files,
git state, TraceDecay stores through supported commands, and remote PR/CI state
when the parent provides that scope. They must not edit files, create commits,
push, merge, release, mutate installations, change daemon state, run database
maintenance, or apply automation artifacts. They hand exact recommended actions
and verification commands to the parent agent, which owns executive decisions
and mutations.

## Testing and adoption evidence

- Bundle contracts prove all eight agents deploy to Claude, Cursor, and Codex
  with no missing support files or host-specific drift.
- Static contracts reject MCP-only instructions, direct database access, and
  mutation language in read-only agents.
- Each new agent gets at least one neutral adoption scenario whose prompt names
  neither TraceDecay nor the agent. Grading records first tool, invoked agent or
  skill, discovery channel, tool budget, and evidence-backed outcome.
- A CLI-only packaging test removes MCP configuration and proves each agent can
  still discover the correct TraceDecay command path.
- Existing plugin validation, formatting, bundle, and host materialization
  suites remain green.

## Non-goals

- One agent per MCP tool.
- Autonomous repair, release, or merge agents.
- Host-specific agent behavior beyond the packaging adapter.
- Replacing workflow skills or explicit commands with agents.
