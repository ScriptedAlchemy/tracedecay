# LSP Full Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the Plan 35 LSP journey with atomic lifecycle cleanup, true retained Tree-sitter overlays, canonical workspace-folder mutation, bounded workspace diagnostics, read-only rename discovery, and real multi-root conformance.

**Architecture:** Extend the existing authenticated daemon-owned LSP actor. The LSP crate retains only client-session state; it consumes `tracedecay-code-extraction` retained parse documents and emits typed workspace-folder mutations that the daemon authorizes through the canonical registered-root locator and persisted scope-set authority. Workspace diagnostics and rename remain read-only provider operations, with exact routed-root identity and typed partial/unavailable outcomes.

**Tech Stack:** Rust 2024, Tokio, serde/serde_json, LSP 3.17, Tree-sitter through `tracedecay-code-extraction`, TraceDecay registered-root and scope-set authorities.

## Global Constraints

- Do not create a parser cache, provider registry, root registry, or scope authority parallel to the canonical owners.
- Dirty overlays use `ParseDocumentIdentity::SessionOverlay`; never fabricate repository ref/commit/tree identity.
- A successful full replacement is `ParseReuse::Reset`, not an error; unsupported language or parse failure preserves the accepted text overlay and reports typed parser state.
- Workspace folder URIs must resolve through registered project identity and `RegisteredScopeResolver`; never fall back to CWD or the active graph.
- Standard `textDocument/rename` must remain unadvertised and unavailable until Plan 34 owns apply; this slice exposes only read-only candidate/preview behavior and never emits `WorkspaceEdit` or `workspace/applyEdit`.
- Bound open documents to 128, overlay bytes to 2 MiB per document, workspace roots to 8, workspace diagnostic fanout to 4, and retained diagnostic results to 128.
- Preserve authenticated request/deadline/cancellation identity across every daemon-routed operation.
- The design/spec documents are implementation notes; durable product truth belongs in Plan 35 and `00-plan-set-index.md`.

---

### Task 1: Reconcile Atomic Session Lifecycle

**Files:**
- Modify: `src/daemon/service/invocation/lsp.rs`
- Modify: `src/daemon/lsp_sessions.rs`
- Test: `src/daemon/service/invocation/tests/dispatch_tests.rs`
- Test: `src/daemon/tests/socket.rs`

**Interfaces:**
- Consumes: `LspSessionRegistry::{reconnect_with_credential,renew,close}`, `DaemonLspProtocolSession::{reconnect,detach,close}`, and daemon-owned invocation task cancellation.
- Produces: one atomic registry/runtime transition for reconnect, renew, detach, exit, expiry, and client disconnect; no detached invocation task survives its owning client.

- [ ] **Step 1: Write failing lifecycle tests**

```rust
#[tokio::test(start_paused = true)]
async fn renewal_keeps_registry_and_runtime_alive_until_the_same_expiry() {
    // Open through the real invocation service, advance beyond the original
    // TTL after renewal, assert both registry access and protocol dispatch
    // remain live, then advance beyond the renewed TTL and assert both expire.
}

#[tokio::test]
async fn reconnect_race_leaves_exactly_one_live_registry_and_runtime_lease() {
    // Race two authenticated reconnects and assert one succeeds, the loser is
    // closed, and final detach removes both registry and runtime ownership.
}

#[tokio::test]
async fn dropped_socket_cancels_and_joins_owned_lsp_invocations() {
    // Drop the real socket client while a controlled invocation is retained;
    // assert cancellation and completion before connection cleanup returns.
}
```

- [ ] **Step 2: Run the focused tests and observe the current split-state failure**

Run: `cargo test -p tracedecay --lib daemon::service::invocation::tests::dispatch_tests::renewal_keeps_registry_and_runtime_alive_until_the_same_expiry -- --nocapture`

Run: `cargo test -p tracedecay --lib daemon::service::invocation::tests::dispatch_tests::reconnect_race_leaves_exactly_one_live_registry_and_runtime_lease -- --nocapture`

Run: `cargo test -p tracedecay --lib daemon::tests::socket::dropped_socket_cancels_and_joins_owned_lsp_invocations -- --nocapture`

Expected: each new test reports one executed failure against the current floor.

- [ ] **Step 3: Apply only the unique lifecycle intent from `9d08805af` and `f4b379c71`**

```rust
// Under the service transition lock:
// 1. authenticate/transition the registry lease;
// 2. transition and renew the actor lease to the same deadline;
// 3. synchronously cancel the replaced lease;
// 4. roll back the registry transition if the actor transition fails;
// 5. remove exited/expired actors explicitly.
//
// The socket owner stores every invocation JoinHandle with its cancellation
// token and drains that set during connection cleanup.
```

- [ ] **Step 4: Run lifecycle tests**

Run the three commands from Step 2 plus:

`cargo test -p tracedecay --lib daemon::service::invocation::tests::dispatch_tests::lsp_disconnect_reconnect_and_final_detach_have_distinct_lifecycles -- --nocapture`

`cargo test -p tracedecay --lib daemon::tests::socket::stdio_bridge_session_reconnects_on_a_fresh_socket_and_resumes_frames -- --nocapture`

Expected: non-zero test count and all pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/service/invocation/lsp.rs src/daemon/lsp_sessions.rs \
  src/daemon/service/invocation/tests/dispatch_tests.rs src/daemon/tests/socket.rs
git commit -m "fix(lsp): make session lifecycle transitions atomic"
```

---

### Task 2: Retain True Incremental Parse Overlays

**Files:**
- Modify: `crates/tracedecay-lsp/Cargo.toml`
- Modify: `crates/tracedecay-lsp/src/overlay.rs`
- Modify: `crates/tracedecay-lsp/src/protocol/lifecycle_controller.rs`
- Modify: `crates/tracedecay-lsp/src/protocol.rs`
- Modify: `crates/tracedecay-lsp/src/lib.rs`
- Test: `crates/tracedecay-lsp/src/overlay.rs`
- Test: `crates/tracedecay-lsp/src/protocol/tests.rs`

**Interfaces:**
- Consumes: `tracedecay_code_extraction::incremental::{ParseDocumentIdentity, ParseInputEdit, ParseLimits, ParsePoint, ParseReport, ParseError, RetainedParseDocument}` with `RetainedParseDocument::open(identity, language_id, source, limits)` and atomic `apply_edits(&mut self, next_identity, edits, new_source)`.
- Produces: `OverlayParseState::{Ready(ParseReport), Unavailable(OverlayParseUnavailable)}` in each overlay snapshot, where unsupported/failed parsing never rejects a valid text change.

- [ ] **Step 1: Write failing overlay behavior tests**

```rust
#[test]
fn ordered_utf16_changes_reuse_one_retained_tree_with_exact_input_edits() {
    // Open Rust text containing a non-BMP character, apply two ordered range
    // changes whose second range addresses the first change's result, and
    // assert final text, Incremental reuse, and bounded changed ranges.
}

#[test]
fn invalid_later_change_rolls_back_text_and_retained_tree_together() {
    // Apply one valid and one out-of-bounds ordered change in one notification;
    // assert version, text, parse identity, and report are byte-for-byte the
    // pre-change snapshot.
}

#[test]
fn unsupported_language_preserves_text_with_typed_parse_unavailable_state() {
    // Open and change a valid overlay using an unsupported language id; assert
    // text/version advance while parse state names UnsupportedLanguage.
}
```

- [ ] **Step 2: Run the tests and observe missing retained parse state**

Run: `cargo test -p tracedecay-lsp overlay::tests::ordered_utf16_changes_reuse_one_retained_tree_with_exact_input_edits -- --nocapture`

Run: `cargo test -p tracedecay-lsp overlay::tests::invalid_later_change_rolls_back_text_and_retained_tree_together -- --nocapture`

Run: `cargo test -p tracedecay-lsp overlay::tests::unsupported_language_preserves_text_with_typed_parse_unavailable_state -- --nocapture`

Expected: each new test reports one executed failure because `DocumentOverlay` has no retained parser/report.

- [ ] **Step 3: Derive edits against evolving temporary text**

```rust
struct PendingOverlayEdit {
    input_edit: ParseInputEdit,
    next_text: String,
}

fn apply_change_and_capture_edit(
    text: &str,
    change: &OverlayChange,
) -> Result<PendingOverlayEdit, OverlayError> {
    // Convert UTF-16 positions to byte offsets against `text`, compute byte
    // points for old and replacement text, construct ParseInputEdit, and
    // return the next text without mutating the live overlay.
}
```

Build the complete ordered batch on temporary text, construct the final `SessionOverlay` identity from exact scope/document/version/content digests, call one atomic `apply_edits`, and publish text/tree/report only after it returns `Ok`.

- [ ] **Step 4: Preserve text on typed parser unavailability**

```rust
pub enum OverlayParseUnavailable {
    UnsupportedLanguage,
    SourceTooLarge,
    InvalidEdit,
    IdentityMismatch,
    GrammarRejected,
    TimedOut,
    ParseFailed,
}
```

On open or change parser failure, accept the already-validated text/version, drop the retained parser if its identity can no longer match, and expose the exact typed unavailable state. Close/expiry drops the retained parser with the overlay.

- [ ] **Step 5: Run overlay and protocol suites**

Run: `cargo test -p tracedecay-lsp overlay::tests -- --nocapture`

Run: `cargo test -p tracedecay-lsp protocol::tests -- --nocapture`

Expected: non-zero counts and all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tracedecay-lsp/Cargo.toml crates/tracedecay-lsp/src/overlay.rs \
  crates/tracedecay-lsp/src/protocol/lifecycle_controller.rs \
  crates/tracedecay-lsp/src/protocol.rs crates/tracedecay-lsp/src/lib.rs
git commit -m "feat(lsp): retain incremental parse overlays"
```

---

### Task 3: Admit Dynamic Workspace Folders Through Canonical Roots

**Files:**
- Create: `crates/tracedecay-lsp/src/workspace.rs`
- Modify: `crates/tracedecay-lsp/src/lib.rs`
- Modify: `crates/tracedecay-lsp/src/dispatch.rs`
- Modify: `crates/tracedecay-lsp/src/protocol/lifecycle_controller.rs`
- Modify: `crates/tracedecay-lsp/src/protocol.rs`
- Modify: `crates/tracedecay-lsp/src/capabilities.rs`
- Modify: `crates/tracedecay-lsp/src/session.rs`
- Modify: `crates/tracedecay-usecases/src/lsp_support/factory.rs`
- Modify: `src/daemon/lsp_sessions.rs`
- Modify: `src/daemon/service/invocation/lsp.rs`
- Modify: `src/daemon/project_open_owners.rs`
- Test: `crates/tracedecay-lsp/src/protocol/tests.rs`
- Test: `src/daemon/service/invocation/tests/dispatch_tests.rs`

**Interfaces:**
- Consumes: `RegisteredRootSelectorV1::new`, `RegisteredRootLocatorV1::new`, `AuthorizedRootAdmission::new`, `AuthorizedScopeSetAuthority::authorize_registered`, `RegisteredScopeResolver::resolve`, `DaemonInvocationService::lsp_owner`, and `compare_and_swap_scope_set`.
- Produces: `WorkspaceFolderMutation { request_id, observed_scope_digest, added: Vec<WorkspaceFolderUri>, removed: Vec<WorkspaceFolderUri> }` plus daemon acknowledgement `WorkspaceFolderMutationOutcome::{Applied(AuthorizedLspWorkspace), Rejected(WorkspaceFolderMutationFailure)}`.

- [ ] **Step 1: Write failing protocol and daemon tests**

```rust
#[test]
fn workspace_folder_notification_emits_one_fenced_mutation_without_local_apply() {
    // Initialize with workspace-folders support, send one add/remove
    // notification, and assert one typed mutation carrying the observed
    // scope-set digest while the actor workspace is unchanged.
}

#[tokio::test]
async fn registered_folder_mutation_persists_and_routes_to_exact_new_root() {
    // Mount two real registered projects, open on root A, add root B through
    // the notification/daemon path, then issue a document request in B and
    // assert B's exact provider and persisted locator served it.
}

#[tokio::test]
async fn stale_or_unregistered_folder_mutation_is_rejected_without_state_change() {
    // Race mutations from the same observed digest and include an unregistered
    // URI; assert typed stale/unregistered failure and unchanged persisted set.
}
```

- [ ] **Step 2: Run the tests and observe explicit-unavailable failures**

Run: `cargo test -p tracedecay-lsp protocol::tests::workspace_folder_notification_emits_one_fenced_mutation_without_local_apply -- --nocapture`

Run: `cargo test -p tracedecay --lib daemon::service::invocation::tests::dispatch_tests::registered_folder_mutation_persists_and_routes_to_exact_new_root -- --nocapture`

Expected: failures show `workspace/didChangeWorkspaceFolders` is explicitly unavailable and no daemon mutation route exists.

- [ ] **Step 3: Parse and queue a typed actor mutation**

```rust
pub struct WorkspaceFolderMutation {
    pub observed_scope_digest: ManifestDigest,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

pub enum WorkspaceFolderMutationFailure {
    InvalidUri,
    UnregisteredRoot,
    AmbiguousRoot,
    StaleScopeSet,
    RootLimitExceeded,
    AuthorizationUnavailable,
}
```

Validate file URIs, duplicates, add/remove overlap, and the eight-root bound in `workspace.rs`. The actor queues the mutation; it does not mutate `AuthorizedLspWorkspace` until the daemon returns a matching fenced outcome.

- [ ] **Step 4: Resolve and persist with canonical authorities**

```rust
// For each candidate URI:
// uri -> canonical absolute requested root
// registry project identity -> registered root/store/profile
// RegisteredScopeResolver::resolve(registered_root, requested_root, project_id)
// RegisteredRootLocatorV1::new(project_id, profile_id, store_id, registered_root)
// AuthorizedRootAdmission::new(request_context, locator)
// AuthorizedScopeSetAuthority::authorize_registered(...)
// compare_and_swap_scope_set(active_root, request, roots, observed_at)
```

Reject missing, ambiguous, mismatched, or stale roots. Apply the returned `AuthorizedLspWorkspace` and provider map together, then invalidate workspace diagnostic results.

- [ ] **Step 5: Advertise only mounted workspace-folder support**

Set `workspace.workspaceFolders.supported` only when the session has the daemon mutation authority and the client advertises workspace folders. Keep it false for fixed/single-provider construction.

- [ ] **Step 6: Run the focused tests**

Run the commands from Step 2 plus:

`cargo test -p tracedecay-lsp protocol::tests::two_root_session_routes_documents_and_workspace_requests_to_exact_roots -- --nocapture`

Expected: non-zero counts and all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/tracedecay-lsp/src/workspace.rs crates/tracedecay-lsp/src/lib.rs \
  crates/tracedecay-lsp/src/dispatch.rs crates/tracedecay-lsp/src/protocol \
  crates/tracedecay-lsp/src/protocol.rs crates/tracedecay-lsp/src/capabilities.rs \
  crates/tracedecay-lsp/src/session.rs \
  crates/tracedecay-usecases/src/lsp_support/factory.rs src/daemon/lsp_sessions.rs \
  src/daemon/service/invocation/lsp.rs src/daemon/project_open_owners.rs
git commit -m "feat(lsp): authorize dynamic workspace folders"
```

---

### Task 4: Add Bounded Workspace Diagnostics

**Files:**
- Create: `crates/tracedecay-lsp/src/workspace_diagnostics.rs`
- Modify: `crates/tracedecay-lsp/src/lib.rs`
- Modify: `crates/tracedecay-lsp/src/dispatch.rs`
- Modify: `crates/tracedecay-lsp/src/protocol/diagnostics_controller.rs`
- Modify: `crates/tracedecay-lsp/src/protocol.rs`
- Modify: `crates/tracedecay-lsp/src/rpc.rs`
- Modify: `crates/tracedecay-lsp/src/capabilities.rs`
- Modify: `crates/tracedecay-usecases/src/lsp_support/runtime_adapters.rs`
- Modify: `crates/tracedecay-usecases/src/lsp_support/factory.rs`
- Test: `crates/tracedecay-lsp/src/protocol/tests/diagnostic_publication.rs`
- Test: `src/daemon/service/invocation/tests/dispatch_tests.rs`

**Interfaces:**
- Consumes: exact authorized roots and each root's canonical `DiagnosticSnapshotPort`; previous result ids are `(uri, resultId)` pairs from LSP 3.17.
- Produces: `WorkspaceDiagnosticOutcome { items, root_failures, complete }`, max four concurrent root reads and 128 retained result identities.

- [ ] **Step 1: Write failing workspace-diagnostic tests**

```rust
#[test]
fn workspace_diagnostic_fans_out_to_exact_roots_and_preserves_partial_failure() {
    // Two routed providers return diagnostics and one returns Unavailable;
    // assert successful items remain, failure carries that root identity, and
    // the response is not fabricated complete.
}

#[test]
fn workspace_diagnostic_previous_result_ids_return_unchanged_reports() {
    // Pull once, pass the literal returned ids back, and assert unchanged
    // reports for matching root/document generations.
}

#[test]
fn folder_mutation_invalidates_only_removed_or_changed_result_identities() {
    // Remove one root after a pull; assert surviving root ids remain reusable
    // and removed-root ids are absent.
}
```

- [ ] **Step 2: Run tests and observe explicit-unavailable response**

Run: `cargo test -p tracedecay-lsp protocol::tests::diagnostic_publication::workspace_diagnostic_fans_out_to_exact_roots_and_preserves_partial_failure -- --nocapture`

Expected: one executed failure because `workspace/diagnostic` is explicitly unavailable.

- [ ] **Step 3: Implement bounded root fanout and result identity**

```rust
pub const MAX_WORKSPACE_DIAGNOSTIC_FANOUT: usize = 4;
pub const MAX_WORKSPACE_DIAGNOSTIC_RESULTS: usize = 128;

pub struct WorkspaceDiagnosticRootFailure {
    pub scope_digest: ManifestDigest,
    pub reason: WorkspaceDiagnosticFailureReason,
}
```

Route each root through its exact provider, admit no more than four reads at once, sort output by scope digest then URI, and emit typed partial failure data rather than empty success. Reject requests exceeding bounds before provider reads.

- [ ] **Step 4: Negotiate the mounted capability**

Advertise `diagnosticProvider.workspaceDiagnostics: true` only when the session has the workspace diagnostic authority. Parse `previousResultIds`, serialize full/unchanged reports, and keep `workspace/diagnostic/refresh` negotiation independent.

- [ ] **Step 5: Run diagnostic suites**

Run: `cargo test -p tracedecay-lsp protocol::tests::diagnostic_publication -- --nocapture`

Run: `cargo test -p tracedecay --lib daemon::service::invocation::tests::dispatch_tests -- --nocapture`

Expected: non-zero counts and all focused LSP diagnostic tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tracedecay-lsp/src/workspace_diagnostics.rs \
  crates/tracedecay-lsp/src/lib.rs crates/tracedecay-lsp/src/dispatch.rs \
  crates/tracedecay-lsp/src/protocol/diagnostics_controller.rs \
  crates/tracedecay-lsp/src/protocol.rs crates/tracedecay-lsp/src/rpc.rs \
  crates/tracedecay-lsp/src/capabilities.rs \
  crates/tracedecay-usecases/src/lsp_support/runtime_adapters.rs \
  crates/tracedecay-usecases/src/lsp_support/factory.rs
git commit -m "feat(lsp): add bounded workspace diagnostics"
```

---

### Task 5: Expose Read-Only Rename Discovery

**Files:**
- Modify: `crates/tracedecay-lsp/src/dispatch.rs`
- Modify: `crates/tracedecay-lsp/src/protocol/semantic_controller.rs`
- Modify: `crates/tracedecay-lsp/src/protocol.rs`
- Modify: `crates/tracedecay-lsp/src/rpc.rs`
- Modify: `crates/tracedecay-lsp/src/capabilities.rs`
- Test: `crates/tracedecay-lsp/src/protocol/tests.rs`
- Test: `src/diagnostics/lsp/semantic.rs`

**Interfaces:**
- Consumes: existing `SemanticRequest::RenameCandidate` and `RenameCandidateResult`.
- Produces: a read-only TraceDecay request result containing exact range, placeholder, evidence identity, and typed unavailable reason; standard `prepareRename` and `rename` remain unavailable and unadvertised.

- [ ] **Step 1: Write failing read-only rename tests**

```rust
#[test]
fn readonly_rename_candidate_routes_to_exact_root_and_serializes_evidence() {
    // Send the read-only method for a document in root B and assert B's
    // provider result contains the exact literal range/placeholder/evidence.
}

#[test]
fn standard_rename_methods_remain_unadvertised_and_never_emit_workspace_edit() {
    // Initialize, assert no renameProvider, send prepareRename and rename, and
    // assert typed unavailable responses plus no workspace/applyEdit message.
}
```

- [ ] **Step 2: Run tests and observe missing read-only dispatch**

Run: `cargo test -p tracedecay-lsp protocol::tests::readonly_rename_candidate_routes_to_exact_root_and_serializes_evidence -- --nocapture`

Expected: one executed failure because the existing gateway candidate is not exposed by protocol dispatch.

- [ ] **Step 3: Route the read-only method**

```rust
pub const TRACEDECAY_RENAME_CANDIDATE_METHOD: &str =
    "tracedecay/textDocument/renameCandidate";
```

Parse a document position, route through the existing semantic controller and exact admitted root, serialize candidate/disagreement/stale/unavailable outcomes, and retain request deadline/cancellation handling. Do not add a mutation port, `WorkspaceEdit`, or apply command.

- [ ] **Step 4: Run rename and semantic tests**

Run: `cargo test -p tracedecay-lsp protocol::tests::readonly_rename_candidate_routes_to_exact_root_and_serializes_evidence -- --nocapture`

Run: `cargo test -p tracedecay --lib diagnostics::lsp::semantic::tests -- --nocapture`

Expected: non-zero counts and all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tracedecay-lsp/src/dispatch.rs \
  crates/tracedecay-lsp/src/protocol/semantic_controller.rs \
  crates/tracedecay-lsp/src/protocol.rs crates/tracedecay-lsp/src/rpc.rs \
  crates/tracedecay-lsp/src/capabilities.rs
git commit -m "feat(lsp): expose read-only rename discovery"
```

---

### Task 6: Prove Full Multi-Root Conformance and Publish Durable Truth

**Files:**
- Create: `tests/lsp_multi_root_journey.rs`
- Modify: `docs/plans/tracedecay-v2/35-daemon-lsp-gateway-and-universal-diagnostics.md`
- Modify or delete: `docs/superpowers/specs/2026-08-04-lsp-full-surface-design.md`
- Modify: `docs/superpowers/plans/2026-08-04-lsp-full-surface.md`

**Interfaces:**
- Consumes: real daemon socket bridge, registered project/store/profile identities, two exact providers, dynamic folder mutation, diagnostics, rename discovery, and overlay lifecycle.
- Produces: one production journey proving admission, routing, mutation, diagnostic partiality, overlay isolation, reconnect, detach, and cleanup across two roots.

- [ ] **Step 1: Write the failing production journey**

```rust
#[tokio::test]
async fn lsp_multi_root_session_preserves_exact_authority_across_full_lifecycle() {
    // Register two isolated project roots and providers.
    // Open one authenticated LSP session with both folders.
    // Open/change documents in both roots and assert retained incremental parse.
    // Pull workspace diagnostics and assert exact root identities.
    // Request read-only rename discovery in root B.
    // Remove B and assert B requests fail closed while A remains live.
    // Reconnect on a fresh socket and assert A state survives.
    // Exit/drop the client and assert registry, runtime, overlay, parse tree,
    // pending requests, diagnostic ids, and provider ownership are released.
}
```

- [ ] **Step 2: Run the journey and observe the first unmet production boundary**

Run: `cargo test -p tracedecay --test lsp_multi_root_journey -- --nocapture`

Expected: one executed failure until all production composition paths from Tasks 1–5 are mounted.

- [ ] **Step 3: Correct composition only at the failing production boundary**

Wire the existing production owner/factory paths needed by the journey. Do not add test-only production ports or fallback resolution.

- [ ] **Step 4: Update durable Plan 35 truth**

Record the shipped lifecycle, retained parsing, folder mutation, workspace diagnostics, and read-only rename discovery in Plan 35. Remove roadmap/floor/future language from the temporary design note, or delete it if Plan 35 now carries every lasting invariant.

- [ ] **Step 5: Run broad verification**

Before Cargo, inspect active builds with:

`ps -eo pid,etime,args | rg '[c]argo (check|test|nextest)'`

Then run:

`cargo fmt --check`

`cargo check -p tracedecay-lsp -p tracedecay-usecases`

`cargo test -p tracedecay-lsp -- --nocapture`

`cargo test -p tracedecay --test lsp_multi_root_journey -- --nocapture`

`cargo check -p tracedecay --all-features`

`git diff --check`

Expected: all commands pass with non-zero LSP test counts.

- [ ] **Step 6: Commit**

```bash
git add tests/lsp_multi_root_journey.rs \
  docs/plans/tracedecay-v2/35-daemon-lsp-gateway-and-universal-diagnostics.md \
  docs/superpowers/specs/2026-08-04-lsp-full-surface-design.md \
  docs/superpowers/plans/2026-08-04-lsp-full-surface.md
git commit -m "test(lsp): prove full multi-root conformance"
```
