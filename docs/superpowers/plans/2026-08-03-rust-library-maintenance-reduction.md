# Rust Library Maintenance-Reduction Execution Note

Status: queued behind the current V2 landing wave.

Authority remains `docs/plans/tracedecay-v2/00-plan-set-index.md`. This note
records the maintained-library cutovers requested for the same final-V2
delivery.

## Constraints

- Plan 39 exclusively owns Grafeo and graph/vector database adoption. Do not
  start a second Grafeo, graph-runtime, graph-SQL, or Petgraph lane here.
- Complete each accepted cutover in place. No dual backend, compatibility
  facade, feature-gated placeholder, or unpublished V1 reader.
- Prefer deletion. Add a dependency only when its production integration
  removes more generic custom machinery than it adds.
- Verify direct production behavior and failure journeys. Do not add
  source-shape scans, test-name inventories, mechanical contract matrices,
  boundary-only tests, or acceptance scaffolding that does not execute the
  shipped path.
- Preserve TraceDecay-owned identity, authorization, cancellation, receipt,
  durability, ordering, retry, and lifecycle semantics.
- Use isolated profiles and sockets. Never run the V2 daemon against the
  operator's live TraceDecay profile.

## Ordered Cutovers

### 1. `tokio-util`

Wait for the MCP deadline and hook lanes to land. Then replace generic
cancellation wait/notify machinery and LSP byte framing where
`tokio_util::sync::CancellationToken` and `tokio_util::codec` preserve the
existing typed behavior. Keep TraceDecay application token identity and strict
LSP bounds, CRLF, duplicate-header, EOF, deadline, backpressure, and ACK rules.
Delete replaced loops and framing code.

### 2. Async Rust SDK

Resume the recognized `v2-sdk-complete` worktree after reconciliation. Remove
blocking `reqwest`; make unpublished SDK operations async; expose
`OperationStream<E>: futures_core::Stream`; use `eventsource-stream` only for
generic SSE framing and `serde_path_to_error` for typed payload diagnostics.
Keep TraceDecay sequence, frontier, correlation, resume-gap, retry, terminal,
and cancellation state machines. Delete manual SSE and blocking iterator code.

### 3. `octocrab` — rejected

Preflight found no deletion-positive cutover. Octocrab's convenience paths
enable or assume redirect/retry/unbounded collection behavior that cannot own
TraceDecay's fixed-route, bounded-body, authorization, ETag/304, rate-limit,
checkpoint, cancellation, and nested pagination semantics. A safe integration
would retain nearly all custom policy/state code, add a larger HTTP/Tower
stack, and still could not remove `ureq` because non-GitHub production callers
remain. Do not add Octocrab or a second GitHub transport. Reconsider only when
the maintained API can materially delete the existing bounded transport.

### 4. `croner`

Rejected after implementation and focused review. Preserving the shipped
five-field numeric grammar, full-range DOM/DOW wildcard behavior, scalar-step
semantics, Sunday `0/7`, UTC behavior, and the exact 367-day lookup bound
required a second normalization/parser layer beside `croner`. The complete
cutover produced no production-code deletion (`+168/-166`) and added `croner`,
`strum`, and `strum_macros`; malformed full-range expressions also bypassed
validation. Keep the existing scheduler and do not add `croner`.

### 5. `binrw`

Wait for the hook lane to land. Use `binrw` for declarative final-V2 durable
frame headers only where it replaces manual offsets and duplicate scanners.
Retain payload bounds, SHA-256 verification, append durability, truncation,
quarantine, and unpublished-tail proofs. Incompatible frames return the
fresh-profile reset outcome; no legacy decoder survives.

### 6. `aide`

Adopt `aide` only for routers and OpenAPI generation backed by canonical
Serde/schemars DTOs and the runtime catalog. It must not own DTOs,
authorization, body limits, retries, cancellation, lifecycle, or error
semantics. Delete duplicate route/schema construction only after shipped HTTP
handlers produce the same authorized operations and typed responses.

## Per-Lane Workflow

1. Inspect the reconciled current worktree and production callers.
2. Research the maintained crate's current official documentation and source.
3. Propose the smallest deletion-positive design with owned files and direct
   behavioral acceptance evidence.
4. Parent review approves or rejects the design before implementation.
5. Implement in an isolated recognized worktree; commit coherent cutovers.
6. Fresh independent review, parent diff review, focused tests, merge, push,
   and delete only the recognized merged worktree.

## Completion Evidence

- Every added dependency has a real production caller.
- Replaced custom production code and obsolete dependencies are deleted.
- Direct malformed-input, denial, cancellation, retry, shutdown/restart, and
  partial-data journeys pass where relevant.
- Workspace/all-feature, dashboard, host/package, isolated runtime, dependency,
  license, vulnerability, and unused-dependency checks pass.
- Final branch review finds no duplicate authority or compatibility residue.
