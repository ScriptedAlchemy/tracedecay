# V2 current outcomes

`00-plan-set-index.md` remains sole roadmap/acceptance authority.
This file records outcomes only. Last reconciled: 2026-08-30.

Branch: `codex/tracedecay-total-redesign-plan-reopened` (PR #707).
Workspace: 38 crates under `crates/` (virtual root; counted from workspace
`members` in the root `Cargo.toml`). Build via plain `cargo` (cargo-conductor
brokers it; see `docs/CARGO-CONTENTION-POLICY.md`). Do not reclaim or wipe
the machine kache store; do not treat sccache or a per-worktree
`CARGO_TARGET_DIR=/tmp/...` as the compile cache.

## Outcomes since 2026-08-19

- Usecase code-index generation retention (journal + receipt store) lives in
  `tracedecay-code-index-retention`, not `tracedecay-usecases/src/retention`.
- GitHub read path: fail-closed REST protocol envelope in
  `crates/tracedecay-usecases/src/advisory/github_runtime/protocol.rs`
  (`Retry-After` / `Link`). One static GraphQL query is unchanged.
- Canonical clock routing: usecases wall-clock reads go through
  `tracedecay-application` `clock` (`now_micros` / `try_now_micros` in
  `crates/tracedecay-application/src/clock.rs`).
- Hook-identity canonicalization: `envelope_identity_hash16` in
  `crates/tracedecay-hooks/src/lib.rs` (`HookHostV1` aliases domain
  `NativeHostIdentityV1`).
- `tracedecay-runtime-core` no longer depends on `tracedecay-lsp`.
- Root decomp: `tracedecay-code-index-runtime` owns the scheduler previously
  at `src/daemon/code_index_scheduler/`. Also extracted: `tracedecay-mcp`,
  `tracedecay-session-temporal-store`, `tracedecay-maintenance`,
  `tracedecay-source-edit`, `tracedecay-host-admission`,
  `tracedecay-daemon-protocol`, `tracedecay-daemon-control`,
  `tracedecay-automation-runtime`.
  `tracedecay-cli` absorbs the work, workflow, remote, and upgrade verbs.
- Daemon lifecycle: start a dead installed unit after update
  (`crates/tracedecay-daemon-control/src/service.rs`); treat `WouldBlock`
  connect as saturation; doctor names a stopped-and-disabled installed unit
  (`crates/tracedecay/src/doctor.rs`).
- Domain move-ups: file-document / extraction / lineage records left domain
  for code-index; root-only graph shapes moved to the root crate.
  `review_labels` is deleted — no current vocabulary; Plan 26 archival
  mentions only.
- Machine compile cache is kache, not sccache.

## 2026-08-29 landing wave

Already on `codex/tracedecay-total-redesign-plan-reopened` when this file was
reconciled:

- Scheduler cluster repair: early publish with serving-seat wait, lock-park
  remount, graph-off memory-pressure rebuilds, bounded activation, ready
  abstain, lock-free reconcile slot, publication identity, text-head reopen,
  and label-move CAS.
- Streaming sealed seat: committed-WAL recovery streams instead of
  materializing.
- Grafeo checkpoints are crash-atomic (out-of-place generation + authenticated
  header flip). Catalog format-version guard is pinned; torn-vector checkpoint
  recovery reopens serving search.
- `GrafeoDB::close` skips the checkpoint when the container is already current.
- Durable Ready receipt is reattached after remount.
- Status reads serve the cached background process sample.
- Transport phases are spanned (daemon wire, LSP outbound, automation/MCP).

## 2026-08-30 landing wave

- Retention convergence landed with a binary handoff; the store was growing
  64.9→83.3 GiB per 2 h on the old binary.
- Deferred-bind proxy (#752).
- `semantic activate` journey shipped with typed revision refusal (#753).
- Vector-commit fix (#754): peak RSS 3044→236 MiB, wall 184→24 s at 120k×768.
- Skew-crash poll-frame fix; the daemon DoS is closed.
- Sealed decode cut: 55.3→29.6 s, RSS 2678→1875 MiB.
- Publication gate split: sealed-store build no longer blocks retrieval;
  verify-once markers fixed the double 84 s verify.
- Typed-park plus self-heal for owner-privacy roots.
- Storage-status blocking fixes: 431 ms warm.
- Session journal paging.
- Redundancy sanitizer coordinate fix.
- Fact-store category and telemetry fixes.
- ANN wiring landed, held on exact-flat pending the latency verdict.
- Seconds-as-micros sweep.
- Wave review verdict: sound; 92 local failures proven environmental.

## Unverified on HEAD

These were remaining work on earlier handoffs and were not re-proven here:

- Physical-daemon memory/session/LCM restart across CLI, MCP, HTTP, and SDKs.
- Operator doctor plus a clean Cursor agents/in-composer install → upgrade.
- Incremental indexing matrix (save, rename, delete, ref switch, overflow,
  cancel, restart).
- npm trusted-publisher OIDC (operator-owned).
