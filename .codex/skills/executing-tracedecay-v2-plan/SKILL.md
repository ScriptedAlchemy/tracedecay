---
name: executing-tracedecay-v2-plan
description: Deliver TraceDecay V2 product work from a named plan item without machine-parsing Markdown or maintaining a parallel authority graph.
---

# Deliver TraceDecay V2 work

The plan is human context, not executable state. Do not parse plan Markdown,
compile a task graph, generate authority JSON, maintain a completion ledger, or
block product work on plan-controller machinery.

## Dispatch contract

1. Start from the PR/item the user names. If none is named, select the first
   unfinished product outcome in the plan's written order using Git history,
   current code, and focused tests as evidence.
2. Translate only that outcome into bounded implementation tasks. Record each
   task in the agent prompt: outcome, exact owned files, dependencies, focused
   test command, and prohibited unrelated edits.
3. Decompose every item across at least two agents or subagents. Give no agent
   a whole large feature. Parallelize only disjoint files; serialize shared
   files and dependency edges.
4. Implement product code first. Planning, receipts, and coordination must stay
   smaller than the implementation they support.
5. Use explicit cancellation only. Do not impose wall-clock, agent,
   no-progress, or workflow timeouts.
6. Independently review the integrated diff, run focused tests, then run the
   smallest relevant broader gate. Fix failures in product code or tests; do
   not add controller machinery to explain them.
7. Treat a passing integrated checkout and its Git commit as completion
   evidence. Report changed product files, tests, remaining blockers, and the
   next product outcome.

## Guardrails

- Do not recreate the deleted plan parser, compiler, authority registry,
  transition state machine, generated dispatch packets, or ledger schemas.
- Do not use Hermes Kanban as authority.
- Do not edit Claude workflow JavaScript while delivering plan items unless
  the user explicitly asks for that JavaScript change.
- Stop only for a real product dependency, destructive action, missing access,
  or an unresolved user decision. Plan-format ambiguity alone is not a blocker;
  inspect code and tests and choose the smallest reasonable product slice.
