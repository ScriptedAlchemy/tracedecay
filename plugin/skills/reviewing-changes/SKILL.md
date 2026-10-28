---
name: reviewing-changes
description: Use when the user asks to review, summarize, or assess the risk of a PR, branch, or working-tree diff. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Reviewing changes

Choose the evidence that answers the review question. Start with an orientation
packet, read the relevant code, and deepen only where behavior or risk remains
uncertain.

## Choose the comparison

- For a PR or committed ref range, call `tracedecay_pr_context` for its compact
  briefing: description, retained discussion and checks, change map, and ranked
  reading entrypoints. Hosted evidence is retained by default. Set
  `refresh_github: true` only when a provider refresh is useful; it refreshes the
  configured matching PR within the request deadline. CI remains retained.
- For an uncommitted change summary, call `tracedecay_commit_context`.
  `staged_only: true` compares pinned HEAD with immutable index blobs. The
  default compares HEAD with captured working-tree bytes, including untracked
  files. This is the net result: a staged edit undone in the working tree does
  not appear in the default summary. Read its comparison and source coverage.
- For structured file/hunk metadata, use `tracedecay_git_diff`. Its
  `working_tree` scope compares index to worktree; `staged` compares HEAD to
  index. Read raw patches with shell `git diff` or `git diff --cached`.
- Use `tracedecay_diff_context` for prospective context on specified files. It
  includes unchanged indexed declarations, possible consumers, and test
  candidates. Use PR or commit context when exact changed declarations matter.

## Read the evidence

Read the briefing's entrypoints with their recorded revision and source span.
For deleted or non-head source, use `tracedecay_source_lines` with the full
commit `revision`, repository-relative `file`, and one-based inclusive
`start_line`/`end_line`. Continue at returned `next_line` with the same revision,
file, and original end line; the reader returns whole lines. Historical reads
use the normal admitted source-service lifecycle and configured path policy.
If a read is unavailable, report the limitation; an authorized shell
`git show '<commit-oid>:path/to/file'` can inspect the same committed source.
Staged blob IDs are provenance, not historical commit selectors; use
`git diff --cached` for their patch text.

When an exact-head graph match supplies a `node_id`, pass it directly to
`source_body`, `impact`, or `test_map`. Graph links cannot prove unindexed or
external consumers safe. Use `view: "symbols"` only when exact symbol
pagination helps the task. The brief's token budget is approximate; inspect
omission counts and `over_budget` before assuming its sections are exhaustive.

Expand relevant discussion through supplied provider URLs. A retained
`body_anchor` identifies provenance; it is not a response-cache `retrieve`
handle. `feedback_expand` requires a server-minted finding expansion handle.
Retained checks are observations, not a live inventory of required checks.

Deepen on risky changes through callers, impact, and test links. Confirm each
candidate finding against concrete behavior and a reachable caller. A scan hit
is a lead: test unwraps and unsafe blocks are not defects by themselves. For
dead-code or ship-risk work, see [safety audit](references/safety-audit.md).

A review alone grants no write authority. Honor writes already authorized by
the user; otherwise read [write authority](references/write-authority.md)
before Git apply, native integration, or worktree cleanup. Structural test
selection belongs to `assessing-impact`; structural mutation belongs to
`editing-safely`.

**Report:** actionable findings with location, failure mode, and evidence, plus
material coverage gaps. Distinguish executed tests from structural test links.
Do not apply changes without write authority.
