# LSP Full-Surface Completion Design

## Objective

Complete the remaining Plan 35 production journeys through the existing
daemon-owned LSP gateway:

- retain Tree-sitter trees for supported unsaved overlays and apply real
  `InputEdit` batches before reparsing;
- support exact, authorized workspace-folder changes and multi-root routing;
- serve bounded workspace diagnostics without converting missing coverage into
  an empty success;
- expose the shipped read-only rename candidate/preview path without returning
  or applying a `WorkspaceEdit`; and
- prove these behaviors through portable protocol tests and real daemon-routed
  journeys.

The implementation extends the current gateway, runtime adapters, and daemon
admission authority in place. It adds no parallel LSP server, parser cache,
root registry, diagnostic store, or edit authority.

## Required integration floor

The implementation starts from the current PR421 floor. Before feature work,
the corrected lifecycle chain is reconciled by behavior, not merged wholesale:

- controlled socket invocations remain owned after the waiting caller drops
  and are cancelled with the last client owner;
- disconnect, reconnect, registry credential rotation, runtime lifecycle, and
  lease replacement share one lock order and cannot detach a new transport;
- reconnect renews both registry and runtime expiry and cancels the old lease;
- normal `shutdown` then `exit` permits explicit terminal transport cleanup;
  and
- lease startup, immediate completion, shutdown admission, and task joining
  remain atomic and bounded.

Only missing behavior is ported from the historical lifecycle commits. Newer
floor implementations and unrelated changes win.

## Incremental overlay parsing

`OverlayStore` remains the sole per-session owner of unsaved document text.
Each supported document additionally owns one retained parser document from
the leaf parsing authority in `tracedecay-code-extraction`.

On open, the overlay validates root containment, version, language, document
count, and byte limit before parsing the exact text. On an incremental
`didChange` batch, the overlay:

1. clones the current text and retained parse state;
2. converts each LSP UTF-16 range against the text produced by the preceding
   edit;
3. records exact byte offsets and byte-column points for Tree-sitter
   `InputEdit`;
4. validates `rangeLength` and the document byte limit;
5. applies all edits to the temporary text;
6. applies the ordered edit batch to the temporary retained tree and reparses
   once with the final source; and
7. publishes text, version, retained tree, parse report, and bounded
   `changed_ranges` together.

A full-text content change produces a typed reset report. Unsupported
languages retain a truthful text-only overlay. Parser unsupported, partial,
timeout, or failure state never discards a valid text update and never becomes
durable clean-generation evidence. Close, expiry, and daemon shutdown drop the
retained tree with the overlay.

Dirty overlay identity uses the exact admitted scope, canonical logical path,
overlay version, and content digest. It never fabricates a ref, commit, tree,
or clean repository identity.

## Workspace authority and folder mutation

Initial multi-root admission continues through the canonical multi-root
locator and persisted scope-set authority. Every requested folder must resolve
to one exact registered project/repository/worktree scope. There is no CWD,
active graph, sibling worktree, or hidden-root fallback.

The protocol actor parses
`workspace/didChangeWorkspaceFolders` once and emits a typed mutation intent.
The daemon service resolves added folders asynchronously through the canonical
registered-root locator, authorizes the new exact scope set, and constructs the
existing provider bundle for each added root. It then applies one fenced actor
mutation:

- additions install the exact root and its semantic, diagnostic,
  cancellation, feedback, and context ports;
- removals cancel root-scoped pending requests, clear root-scoped overlays and
  diagnostic publications, and remove the provider ports;
- duplicate additions, unknown removals, capacity overflow, stale mutation
  revisions, ambiguous roots, and authorization loss leave the prior workspace
  intact; and
- the actor never enumerates or discloses roots not named by the client and
  authorized by the daemon.

The workspace and federated provider maps are one mutable authority with a
monotonic session-local revision. A document request resolves to the deepest
exact admitted root. Equal-depth ambiguity and outside-root requests fail
closed. Root order for fan-out and identity is deterministic by authorized
scope digest, not client folder order.

## Workspace diagnostics

Workspace diagnostics are negotiated only when the client supports pull
diagnostics and the daemon mounted the bounded workspace diagnostic authority.
The server capability advertises
`diagnosticProvider.workspaceDiagnostics: true` only in that case.

The request parser accepts bounded `previousResultIds` and standard progress
tokens. The actor snapshots the current workspace revision and fans out across
at most eight admitted roots with at most four concurrent root operations.
Each root uses the same canonical diagnostic provider and generation identity
as document diagnostics. Results are serialized as standard full or unchanged
workspace document reports in deterministic root/document order.

The authority applies explicit document, item, and byte budgets. A stale
workspace revision, authorization change, root failure, indexing state,
timeout, cancellation, or saturation returns typed partial/unavailable
evidence; it never becomes a complete empty report. Previous result IDs are
honored only when their root, document, generation, content, and workspace
revision still match. Unsaved overlay diagnostics are visible only to their
owning session.

## Read-only rename candidate

The standard mutable rename journey remains disabled. The gateway does not
advertise `renameProvider`, return a `WorkspaceEdit`, invoke
`workspace/applyEdit`, or call any Plan 34 apply operation.

The existing daemon semantic authority remains the only rename evidence path.
It queries analyzer and graph candidate evidence under the exact routed root.
Only exact agreement on document, range, and placeholder returns an available
read-only candidate/preview. Missing, stale, partial, unsupported, or
disagreeing evidence returns its existing typed unavailable state. Multi-root
requests route by the candidate document and cannot combine evidence across
roots.

The future apply-grade authority may revise this boundary only after Plan 34
provides one callable, journaled, stale-checked transaction. This delivery
creates no compatibility stub for it.

## Bounds and failure behavior

- JSON-RPC frames remain limited to 4 MiB.
- Unsaved documents remain limited to 2 MiB and 128 open overlays per session.
- Workspaces remain limited to eight exact roots.
- Root fan-out remains limited to four concurrent operations.
- Pending requests, diagnostic items, serialized bytes, and previous result
  identities remain explicitly bounded.
- All production failures are typed. There is no panic, fabricated default,
  empty success, swallowed error, widened timeout, or silent single-root
  fallback.
- Workspace mutation, rename preview, and diagnostics are read-only with
  respect to source, Git, external services, tasks, and durable evidence.

## Direct acceptance

Tests are added before each production change and must fail for the missing
behavior:

1. Overlay tests prove ordered UTF-16 edits yield exact byte/point
   `InputEdit`s, reuse the prior tree, publish bounded changed ranges, roll
   back an invalid later edit, reset on full replacement, isolate clients, and
   release trees on close/expiry.
2. Protocol tests prove exact multi-root initialization, deepest-root routing,
   folder add/remove, stale mutation rejection, hidden-root denial, root
   cleanup, and deterministic order.
3. Workspace diagnostic tests prove full/unchanged responses, exact previous
   result identity, multi-root fan-out, partial root failure, cancellation,
   saturation, bounds, overlay isolation, and no fabricated empty success.
4. Rename tests prove exact analyzer/graph agreement, multi-root routing, stale
   and disagreement denial, and absence of `renameProvider`/`WorkspaceEdit`.
5. Daemon journeys use real registered roots and production provider adapters;
   they do not use a synthetic lookalike server for admission or routing.
6. Existing reconnect, shutdown/exit, cancellation, framing, diagnostics,
   navigation, and context-extension suites remain green.

## Delivery sequence

1. Reconcile and checkpoint the lifecycle corrections on the current floor.
2. Integrate the leaf retained-parser API and checkpoint overlay behavior.
3. Integrate the canonical registered-root locator result, then checkpoint
   workspace mutation and full multi-root conformance.
4. Add bounded workspace diagnostics and checkpoint the daemon journey.
5. Complete read-only rename candidate routing and final cross-feature
   verification.

Generated dashboard contracts are outside this surface and must not be edited.
