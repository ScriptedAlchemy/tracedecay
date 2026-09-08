---
name: tracedecay-dev-sweep
description: "Run a requested broad TraceDecay self-improvement audit across usage, transcripts, automation, hints, and code health."
---

# TraceDecay Development Sweep

Bound the project set, time window, and requested outcome. Select the evidence
lanes that answer it; do not invoke every skill or build the repository merely
to begin an audit. Existing build failures can limit runtime verification but
do not invalidate historical usage or static evidence.

## Evidence lanes

- Usage and adoption: `introspecting-tracedecay-usage`.
- Repeated transcript friction: `self-improving-from-usage-logs`.
- Automation outcomes: `inspecting-automation-cycles`.
- Hook relevance and repetition: `evaluating-hook-hints`.
- Implementation hotspots: `tracedecay:code-health`.

For a full sweep, cover each lane without duplicating evidence collection:
usage owns aggregate metrics, transcript mining owns concrete task examples,
and the other lanes investigate their respective behavior. Parallel agents may
inspect independent bounded lanes with disjoint ownership; the lead combines
findings. Read only each selected skill's relevant references.

## Synthesize and act

Rank findings by evidence, user impact, and the smallest change that addresses
the shared cause. Low usage alone is not failure, and an uncited anecdote does
not justify a broad rule. Report unavailable lanes and continue independent
ones rather than abandoning the sweep.

Apply improvements within the user's scope. Keep code ownership explicit in
shared checkouts and follow repository worktree/build rules. For source skill
edits, reconcile `.agents/skills`, `.codex/skills`, and `.claude/skills` copies
that exist; preserve host metadata. For profile-managed skills, use
`writing-agent-managed-skills` and the canonical administrative/writer path.
An audit does not itself authorize deleting memory or changing schedules.

Verify each change through the narrowest relevant behavior or packaging check.
Reuse passing evidence; run affected suites when the change requires them.
Record durable facts only when useful and authorized under the memory workflow.

Finish with evidence per examined lane, applied fixes and verification,
recommendations or deferred findings, and material coverage limitations.
