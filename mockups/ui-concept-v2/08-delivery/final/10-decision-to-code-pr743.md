---
design_status: current
evidence_class: real_evidence_reconstruction
source_html: 10-decision-to-code-pr743-source.html
---

# PR #743 Decision to Code

Rendered plate: [10-decision-to-code-pr743.png](10-decision-to-code-pr743.png). Deterministic review source: [10-decision-to-code-pr743-source.html](10-decision-to-code-pr743-source.html).

## User job

Answer, with inspectable sources: what was requested, what persisted evidence records the implementation decision, which exact YAML changed, what the workflows actually exercised, what remains risky, and how the result reached both `master` and PR #707.

## Verified scope

- PR #743: `ci(hotpath): enable pull_request profiling with availability guards`.
- Commit `39e4704e0f8c24811f851eecc693cd514401e858`; merge commit `5ca74467e577450a64e820c10e6619ac7919bd04`.
- Branch `ci/hotpath-pr-trigger` to `master`; opened 2026-08-27 21:42:47Z and merged 21:52:18Z.
- Exactly `.github/workflows/hotpath-profile.yml` (+96/−10) and `.github/workflows/hotpath-comment.yml` (+28/−7), total +124/−17.
- Recovered Claude Fable 5 root session `caf0eec6-9fd3-4927-9e33-454e6b627137`, linked to hosted session `session_01CPdGeG8tU25R3QSh5dE7XW`.
- Supporting benchmark agent and nested API researcher are prerequisite/evidence branches; neither authored PR #743's two workflow hunks. No Codex producer attribution is shown.

## Interaction contract

- Eight sourced episodes move from user request to directive, delegation, prerequisite, decision and code, run evidence, review findings, and merge or #707 mirror.
- Previous and Next, clickable episode nodes, and `Alt+Left` or `Alt+Right` provide step navigation.
- `Focus code` or the `F` key gives the diff the full review workspace. Journey and Evidence rails collapse independently with controls or `J` and `E`.
- At narrow or 200% zoom layouts, the panes reflow vertically; code focus hides side context without removing it from the document.
- The code pane scrolls long exact lines instead of clipping them. Exact transcript and evidence-table fallbacks remain explicit actions.
- Local review actions attach comments or challenges to visible persisted artifacts. Provider mode remains read-only.

## Source classes and truth boundary

The interface distinguishes user transcript, assistant summary, subagent report, commit or PR rationale, repository diff, GitHub check result, review finding, and unavailable private reasoning. Persisted messages and summaries are not hidden chain-of-thought.

The observed profile run exercised only `head_bench_available=false`; no timing or workload JSON existed, and no full comparison was proven. The pre-merge companion comment run used the old default-branch workflow and failed on missing base timing. Later guards skipped tooling and comment. The three unresolved findings remain visible: attacker-controlled comment target, unequal digests that only warn, and missing pipefail around timeout through `tee`.

## Scale, access, and implementation

- Large histories virtualize semantic episodes and branch groups rather than shrinking text or rendering every turn.
- Journey and evidence panes are resizable/collapsible; the central diff can claim the entire workspace.
- Keyboard order, 200% zoom, reduced motion, text/table fallbacks, and non-color source labels are required implementation acceptance criteria.
- The checked-in HTML is an isolated concept source used to render the deterministic plate. It is not a production UI or runtime integration.

## Production authorities

- The recovered Claude transcript owns persisted user messages, assistant summaries, subagent reports, timestamps, and transcript availability; private provider reasoning remains unavailable.
- Local Git and the exact repository diff own the two workflow files, line changes, commit identities, merge identity, and the #707 mirror.
- GitHub PR/check evidence owns opened/merged times, check outcomes, artifacts, and review findings; provider actions remain read-only.
- The Decision-to-Code reconstruction links these source classes chronologically while preserving exact facts, explicit persisted rationale, inferred relations, ambiguous attribution, and unavailable evidence. It is a best-available reconstruction, never a hidden thought trace.
