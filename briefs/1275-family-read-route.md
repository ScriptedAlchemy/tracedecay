# Spine unit: Shared Code / Compare read authorities (unblocks #1275)

**Do not spawn until #1269 has an open PR** (same family surface; path-isolate after that PR exists).

## Why
PR #1315 closed as mountless. #1275 UI stays typed-blocked until dashboard transport can read:

1. Shared-family HTTP read — verified exact families (`SimilarFamilyV1` / coverage from #1312, plus #1269 repository redundancy ranking once merged). Not near_pairs.
2. Revision-pair union-layout read — two revisions in one stable union for Compare.

This is **spine**, not dashboard. #1275 UI reopens only after this read path merges.

## Constraints
- Reuse family wire; do not resurrect grep-analysis `RedundancyRequestV1` (#1316 deletes it).
- Dashboard `contracts:generate` if schemas change; never hand-edit `dashboard/src/contracts/`.
- Worktree: `scripts/agent-worktree.sh /fast/tmp/td-1275-family-read -b agent/1275-family-read origin/codex/tracedecay-total-redesign-plan-reopened`
