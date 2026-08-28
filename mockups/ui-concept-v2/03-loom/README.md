# Loom concept plates

## Purpose

Loom is TraceDecay's horizontal temporal execution weave: a reviewer can follow loaded work, replay meaningful events, understand agent/subagent branching, inspect exact evidence, and attach local feedback without turning raw logs into an invented story.

Route: `/loom`.

## Authoritative final set

The canonical product sequence is indexed in [`final/README.md`](final/README.md):

1. [Follow loaded tail](final/01-follow-loaded-tail.md)
2. [Temporal replay](final/02-temporal-replay.md)
3. [Branching execution](final/03-branching-execution.md)
4. [Dense 100+ agents](final/04-dense-100-agents.md)
5. [Selected event evidence](final/05-selected-event-evidence.md)
6. [Feedback continuation](final/06-feedback-continuation.md)
7. [Evidence gaps](final/07-evidence-gaps.md)

These final states replace the v7 vertical host-weave overview as implementation reference. All final plates remain explicitly `CONCEPT / SYNTHETIC DATA` until bound to authenticated production evidence.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- `dashboard/src/workspaces/loom/WeaveCanvas.tsx`, `dashboard/src/workspaces/loom/ThreadPlayback.tsx`, `dashboard/src/workspaces/loom/ThreadChain.tsx`, and `dashboard/src/workspaces/loom/LoomPage.dom.test.tsx` identify the current implementation surface. They do not constrain the final interaction to the legacy vertical renderer.
- A production journey projection must preserve stable event/source identities and the evidence ladder: exact, explicit, inferred, ambiguous, stale, and unavailable.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
