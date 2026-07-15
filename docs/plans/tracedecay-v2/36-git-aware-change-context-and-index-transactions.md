# Git intelligence and safe repository operations

Status: planned across PR7, PR9, PR11, and PR12

## Outcome

TraceDecay makes repository state useful to agents without becoming a Git
implementation or an unrestricted Git command runner. Native Git remains the
authority for repository objects, refs, the working tree, the index, attributes,
ignore rules, and commit creation. TraceDecay adds generation-bound provenance,
typed read-only intelligence, and three narrowly authorized index mutations:
`stage_hunks`, `unstage_hunks`, and `commit_index`.

Every mutation is previewed from an exact repository snapshot, checked again at
apply time, serialized by the daemon, and returned with a durable receipt. A
stale preview never edits the index. CLI and MCP expose the same application
operations, schemas, errors, and receipts.

## Boundaries

This plan does not create:

- a shadow Git object database, index, ref store, history model, or patch engine;
- a generic `git exec`, arbitrary subprocess, or user-supplied Git argument path;
- autonomous merge, rebase, cherry-pick, revert, ref movement, history rewrite,
  fetch, pull, push, branch deletion, tag mutation, or remote mutation;
- implicit staging, committing, conflict resolution, or checkout changes; or
- a claim that graph or session evidence overrides native repository state.

For excluded operations TraceDecay may produce read-only plans, dependency and
impact analysis, predicted conflicts, affected tests, and verification guidance.
It never turns that evidence into mutation authority.

## Delivery ownership

### PR7: repository provenance

PR7 records canonical repository identity and immutable source provenance on
captured observations and published generations. Provenance includes repository
identity, checkout/worktree identity, canonical root, current ref when attached,
HEAD object ID, index tree identity when available, path identity, dirty-state
classification, and capture time. Missing, unborn, detached, conflicted, or
partially readable state is represented explicitly rather than guessed.

PR7 is evidence only. It does not add status, diff, staging, or commit tools and
does not copy Git objects into TraceDecay storage.

### PR9: read-only Git intelligence

PR9 adds typed application operations for:

- repository status, including staged, unstaged, untracked, ignored, renamed,
  conflicted, submodule, sparse-checkout, and file-mode state;
- staged and unstaged diffs with file and hunk structure;
- bounded commit history and commit/object metadata;
- blame/line provenance with boundary, rename-following, and unavailable states;
- hunk intelligence that relates changed spans to symbols, callers, affected
  files, diagnostics, tests, ownership, and source generations; and
- read-only plans for excluded Git operations, including explicit preconditions,
  likely conflicts, impact, and verification evidence.

These operations use native Git plumbing through a fixed internal adapter. They
accept typed inputs, never raw flags, preserve Git's path and encoding behavior,
bound output and traversal, and report unsupported repository states truthfully.

### PR11: daemon-serialized index transactions

PR11 adds exactly three write operations:

- `stage_hunks`: apply selected working-tree hunks to the index;
- `unstage_hunks`: restore selected index hunks to the current HEAD/index base;
  and
- `commit_index`: create one commit from the exact previewed index tree and
  advance the explicitly validated current branch through native Git.

All three enter one daemon-owned per-repository mutation queue. Clients, hooks,
CLI, MCP, and plugins never open or mutate the index directly. The daemon uses
native Git's index transaction mechanisms and repository metadata, revalidates
the expected state immediately before mutation, and publishes one success or
failure receipt before admitting the next mutation. Process failure recovery
compares native Git state with the transaction journal and reports whether the
operation committed, did not commit, or requires user inspection; it never
replays an ambiguous mutation.

`commit_index` accepts structured author/committer identity policy, a validated
message, optional signing policy, and the expected parent/ref state. It cannot
amend, create a merge commit, use arbitrary parents, bypass hooks or signing
policy, stage additional files, or push. Hook failure, signing failure, changed
index state, or changed ref state fails without reporting success.

### PR12: shared CLI and MCP surface

PR12 binds the PR9 and PR11 application operations into the shared tool catalog.
CLI and MCP use the same request and response types, enum values, defaults,
limits, capability metadata, Markdown rendering, JSON rendering, privacy
classification, and stable error taxonomy. Neither transport contains Git
logic, opens repository internals, accepts opaque Git arguments, or implements a
fallback mutation path when the daemon is unavailable.

## Repository snapshot identity

Every read result and write preview carries a `RepositorySnapshot` containing:

- canonical project, repository, and checkout/worktree identity;
- object format and repository-format capabilities;
- HEAD state, attached ref and ref object ID when present;
- index checksum and materialized index-tree object ID;
- relevant worktree file identity, content digest, mode, and stat evidence;
- attributes, ignore, sparse-checkout, submodule, and case-sensitivity context
  needed to interpret selected paths; and
- conflict/unmerged stages and in-progress native Git operation state.

The adapter obtains this evidence from native Git. TraceDecay stores only the
bounded typed result and provenance needed for comparison and audit. A snapshot
with unreadable state, unresolved conflicts, split-index incompatibility, or an
unsupported repository capability remains usable for safe read operations when
truthful, but is ineligible for mutation.

## `HunkRef` compare-and-swap contract

A hunk selected for mutation is identified by an immutable `HunkRef`, not a
display ordinal or line number alone. It contains:

- repository and checkout/worktree identity;
- operation direction: working tree to index or index to HEAD/base;
- canonical path and old/new path for a rename or copy;
- expected base blob object ID or explicit absent-file state;
- expected index blob object ID, mode, and unmerged-stage state;
- expected working-tree content digest and mode when the operation reads it;
- normalized hunk header, context digest, patch digest, and selected line bitmap;
- attributes/filter identity relevant to clean/smudge and end-of-line handling;
  and
- the preview ID, schema version, and snapshot digest that issued the reference.

Preview computes the exact candidate index tree in memory through native Git.
Apply performs compare-and-swap validation for every `HunkRef`, the complete
index, HEAD/ref state, repository operation state, and policy revision. Any
changed precondition rejects the entire transaction. TraceDecay never relocates
a hunk by fuzzy context, silently refreshes it, or partially applies the
remaining references.

Binary changes, submodule entries, intent-to-add entries, conflict stages,
symlinks, file-mode-only changes, renames/copies, filters, and sparse paths each
have explicit capability states. A kind without a proven round-trip adapter is
read-only and cannot produce an applicable `HunkRef`.

## Preview, apply, and receipts

Each write has separate preview and apply phases. Preview is immutable and
returns:

- request, policy, repository snapshot, and selected `HunkRef` digests;
- exact affected paths and old/candidate index-tree IDs;
- rendered patch plus structured file/hunk records;
- symbol, caller, diagnostic, affected-test, and privacy summaries;
- hook/signing requirements for commit preview;
- blocked and unsupported conditions; and
- a preview ID, expiry policy, and content-addressed preview digest.

Apply accepts only the preview ID and digest plus explicit user authorization.
It revalidates the complete preview, executes the native Git transaction, then
returns a receipt containing:

- operation ID, request ID, actor/transport class, and timestamps;
- old and new index-tree IDs, HEAD/ref IDs, and selected `HunkRef` digests;
- native Git outcome, hook and signing outcomes, and created commit ID if any;
- changed paths and the final repository snapshot digest;
- verification evidence and warnings; and
- a receipt schema version and integrity digest.

Dry-run uses the same preview and validation path and emits no apply receipt.
Cancellation before native commit leaves state unchanged. Cancellation after a
native transaction reaches its commit point returns the committed receipt; it
must not report cancellation as if no mutation occurred.

## Failure semantics

Stable failures distinguish at least:

- stale HEAD, attached ref, index, file, mode, attributes, or policy state;
- stale, unknown, expired, malformed, or wrong-repository preview/`HunkRef`;
- ambiguous path identity, case collision, rename/copy ambiguity, or symlink
  escape;
- conflicts, unmerged index stages, or an in-progress Git operation;
- unsupported object format, repository extension, filter, binary operation,
  sparse path, submodule mutation, or file kind;
- partial-hunk selection that cannot form a valid patch;
- native index transaction, hook, signing, identity, message, or ref-update
  failure; and
- daemon unavailable, queue unavailable, authorization denied, cancellation,
  or indeterminate crash recovery.

Failures include safe current-state evidence and a re-preview instruction but
never mutate by retrying with relaxed checks. No successful response is emitted
until native Git state matches the receipt.

## Privacy and authorization

Diffs, untracked content, commit messages, author identities, blame output,
remote URLs, and path names are independently classified. Default rendering
redacts secrets and sensitive paths, bounds context, omits untracked file bodies,
and sanitizes remote credentials. Telemetry records operation kind, latency,
counts, typed failure reason, and capability usage; it does not record patch
content, commit messages, identities, repository URLs, or path names.

Read authorization is path- and repository-scoped. Mutation additionally
requires an explicit capability for the exact operation and repository, a live
preview authorization, and daemon policy approval. Receipts retain digests and
minimal audit metadata under configured retention; sensitive rendered evidence
is not made durable by default.

## Exhaustive acceptance matrix

Acceptance requires fixtures and end-to-end tests for:

- clean, dirty, detached, unborn, bare, linked-worktree, submodule,
  sparse-checkout, ignored, untracked, renamed, copied, deleted, conflicted,
  executable-bit, symlink, binary, non-UTF-8 path, CRLF, filter, and large-file
  repositories on every supported platform and object format;
- staged, unstaged, and mixed changes; multiple hunks in one file; partial-line
  selection rejection; no-newline markers; overlapping selections; and
  rename/mode/content combinations;
- deterministic status, diff, history, blame, hunk ordering, pagination,
  truncation, Markdown/JSON parity, and graph-enrichment provenance;
- every `HunkRef` field drifting independently between preview and apply, with
  proof that the index and ref remain byte-for-byte unchanged;
- concurrent clients previewing and applying overlapping and disjoint changes,
  queue ordering, fairness, cancellation at every boundary, daemon restart, and
  crash recovery before and after the native transaction commit point;
- successful and failing hooks, signing, author policy, empty index, empty
  message, changed parent/ref, protected branch, commit race, and exact created
  commit/tree/parent verification;
- rejection of arbitrary arguments and every excluded mutation through CLI,
  MCP, daemon, malformed transport payloads, and direct client attempts;
- privacy redaction, secret fixtures, untracked-content defaults, telemetry
  minimization, authorization denial, cross-repository replay, and receipt
  retention; and
- stock-Git differential tests proving candidate index trees and commits match
  native Git, plus property and fault-injection tests proving all-or-nothing
  mutation and truthful receipts.

## Acceptance

This plan is complete only when native Git remains the observable authority;
PR7 provenance is generation-bound; PR9 intelligence is read-only and truthful;
PR11 exposes only the three daemon-serialized mutations with `HunkRef`
compare-and-swap; PR12 provides schema-identical CLI/MCP behavior; stale or
unsupported state fails closed; privacy defaults hold; crash recovery is
unambiguous; and the full acceptance matrix passes on supported platforms.
