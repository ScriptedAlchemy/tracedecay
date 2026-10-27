---
name: reviewing-changes
description: Use when the user asks to review, summarize, or assess the risk of a PR, branch, or working-tree diff. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Reviewing changes

Follow these steps in order. Report a step you skip as `skip: <reason>`.

1. For a PR or ref range, call `tracedecay_pr_context` for its compact briefing:
   description, retained discussion and checks, change map, and reading order.
   For working-tree or staged changes, `tracedecay_git_diff` returns bounded
   file/hunk metadata. Read raw patch text with shell `git diff` or
   `git diff --cached` respectively. Call `tracedecay_diff_context` only when
   file-scoped consumers or test candidates would help. It includes unchanged
   declarations in those files.
2. Read the actual changed code at the briefing's entry points, then expand
   relevant discussion, consumers, and tests. Check revision and source coverage:
   retained checks are not a live required-check inventory, and graph links
   cannot prove unindexed or external consumers safe. Use `view: symbols` only
   when exact symbol enumeration helps the task. Use a returned `node_id` for
   current-head `source_body`, `impact`, or `test_map` reads. Read deleted or
   non-head code with shell `git show '<commit-oid>:path/to/file'` at its recorded
   revision, then select the recorded span. Current-source tools do not expand
   arbitrary historical Git blobs. A retained `body_anchor` is
   provenance, not a response-cache `retrieve` handle; use a supplied discussion
   URL or an authorized finding's expansion handle for further evidence.
3. Deepen on the riskiest changed symbols with callers, impact, and test links
   rather than repeating every analysis across every file. skip: the diff is
   small enough to read whole.
4. Confirm each candidate finding against concrete behavior and a reachable
   caller. A scan hit is a lead: test unwraps and unsafe blocks are not defects
   by themselves. For dead-code or ship-risk work, see
   [safety audit](references/safety-audit.md).
5. Explain each finding against the changed behavior and its evidence.

A review grants no write authority. Before a Git apply, a native integration,
or a worktree cleanup, read [write authority](references/write-authority.md).
Structural test selection belongs to `assessing-impact`; structural mutation
belongs to `editing-safely`.

**Report:** actionable findings only, each with location, failure mode, and
evidence, then the coverage the semantic context lacked. Do not post, resolve,
or apply anything the review did not authorize.
