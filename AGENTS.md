# Agent Notes

## Local Cargo Development (Zack's Machine Only)

- This section is an agent/workspace convention for developing TraceDecay in Zack's local
  checkouts. It is not a TraceDecay product requirement, public contributor requirement,
  published Cargo configuration, or hosted-CI policy.
- Do not encode these machine-specific paths or cache choices in tracked product behavior,
  repository Cargo configuration, public documentation, or CI solely to satisfy this section.
- Portable repository Cargo changes are allowed when measurements justify them,
  including manifests, profiles, features, build settings, and build-script
  configuration. Preserve stock-Cargo contributor, CI, release, and published
  package behavior; never hard-code this machine's target paths or slot policy.
- Invoke ordinary `cargo` commands; no build shim or cargo-slot is installed
  (an earlier shim was removed by explicit direction). Concurrent agents share
  this checkout's repo-local `target/`, so waiting on cargo's directory lock is
  normal contention rather than a hang, and a long wait under many concurrent
  Rust processes is expected. Never kill another agent's cargo process, and do
  not add repository lane coordination.
- Do not pause, kill, or disable Rust Analyzer to improve build timings. Its
  Claude Code LSP-owned processes are outside repository build optimization.
- Agents do not set `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` to manage
  contention; scope checks and tests narrowly instead.
- Cargo-launched TraceDecay test data follows the repo-local target. Never
  redirect targets or test data under `/tmp`, `$HOME`, or the root disk.
- During development, scope checks and test compilation to the smallest touched
  package, target, and feature set. A test-name filter does not reduce which
  test binary Cargo compiles, so batch focused tests by target where practical.
- Before handoff, run the relevant broader all-feature gate from the repo root:
  `cargo check --all-features`, `cargo test --all-features`, `cargo test-all`,
  or `cargo nextest run --workspace --all-features --no-fail-fast`.
- Toolchain caches (`sccache`, cargo registry) live under `/fast/cache/` and need no
  per-agent changes.
- Hosted CI and other developers follow their own environment/repository defaults; never
  assume this machine's `/fast` layout exists elsewhere.

## Learned User Preferences

- Do not merge a batch of PRs until aggregate verification is stable; a single flaky pass is not enough.
- Prefer GPT-5.6 Sol as the lead/orchestrator for design, review, and verification; use Cursor Grok or GPT-5.6 Terra as scoped implementers, matching model capability to task intelligence needs; delegate token-heavy evidence gathering while the lead independently verifies edits, synthesizes findings, and makes final judgments.
- When orchestrating parallel agents, the lead dictates exact scoped edits, subagents do not invent scope, and the lead reviews their work before any push.
- When dogfooding TraceDecay, use the repository's official dogfood/install-or-upgrade flow, include migrations and plugin refreshes, improve the dogfooding skill when gaps surface, and verify the installed runtime; never hand-edit installed plugin files.
- For dashboard/frontend work, pursue a distinctive industry-leading interface grounded in researched references and conceptual art; render the code graph as an elegant, futuristic topography spanning files, functions, types, and call paths, and enforce quality with responsive screenshot and accessibility audits.
- In shared dirty checkouts with concurrent agents, work in-place (do not create worktrees), re-read files immediately before editing, and stage, commit, and push only changes made for the current task.
- For provider and observation acceptance, treat only checked-in real fixtures as binding evidence; reject synthetic, lookalike, or invented protocol fields. Agent-adoption evals should measure whether models naturally select TraceDecay skills and tools rather than explicitly requiring their use.
- Prefer mature maintained libraries and existing TraceDecay authorities over custom parsers, cursors, caches, retries, transports, or registries; keep custom code to TraceDecay-specific authorization, scope, and composition.
- Keep plans delivery-first without thinning product scope: fold retained capabilities into runnable journeys, remove only duplicated prose, planning bureaucracy, and scaffold-only milestones, and retain uncertain requirements with an owner. Once a slice is complete, update its authoritative status and historical language so later audits preserve behavioral requirements and future seams without resurrecting superseded scaffolds or validations.
- Never create or recreate PR-specific acceptance snapshots, owner receipts, gate manifests, clean/content-addressed checkout snapshots, signatures, attestations, reveal/trust-root evidence, or giant gate scaffolds. Acceptance is direct product tests plus simple Linux-only developer benchmarks/evals with truthful pass/fail/pending summaries; default-feature product support remains covered by Linux/macOS/Windows CI. Preserve product-runtime receipts for atomic effects, migrations, Git transactions, daemon operations, and rollback, plus immutable code/vector/session generation identity and real source/content digests.
- Avoid privacy or security machinery without a concrete boundary. Retain exact project/user isolation, remote/network authentication, secret prevention, and destructive-operation CAS, confirmation, and rollback, but do not add local first-party signatures, trust roots, or attestations.
- Before starting a broad Cargo check or test, reuse or watch an equivalent relevant run already active in the checkout when possible, and never kill another agent's Cargo process.

## Git

- Every non-merge commit subject must pass `scripts/check-conventional-commits.sh` before push.
- Use `<type>: <subject>` or `<type>(<scope>): <subject>` with one of:
  `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Keep the subject at 72 characters or fewer. Example: `fix(doctor): avoid false orphan warnings`.

## Learned Workspace Facts

- Parallel branch work uses git worktrees under `.worktrees/` in the repo root (for example `.worktrees/codex-cli-args-stdin`); aggregate multi-PR verification uses a detached temporary worktree on `origin/master`, merges all target branches, then runs ordinary Cargo tests.
- Integration/default branch is `master` (GitHub: ScriptedAlchemy/tracedecay).
- Cursor's TraceDecay plugin uses the MCP key `tracedecay`; Claude and Codex retain the `graph` key.
- V2 Plan 35 assigns the daemon the LSP gateway/broker and an intentional versioned `experimental.tracedecay` context projection channel over standard LSP/JSON-RPC; PR12 projects diagnostics, impact, and test context, while later providers advertise only once mounted. Claude Code connects through one configured-language plugin, while non-LSP hosts receive equivalent evidence through hooks, hints, or MCP.
- V2 Plan 37 is the architectural center for branch-aware feedback cycles, read-only GitHub PR review-comment ingestion/surfacing (never posting, updating, resolving, or replying), and concurrent-agent proximity; its PR11–PR13 milestone ships post-edit diagnostics and impact, CI failure localization, review ingest/display, and tiered proximity, while later work adds dashboard/Doctor, multi-root, and remote composition without GitHub writes.
- V2 Plan 27 PR6 owns the host-neutral integration catalog model and observation adapters; PR13 owns packaging, registration, lifecycle, and equivalent capability delivery across first-party host façades, with shared skills kept synchronized and host-specific adapters only where native surfaces differ. Every Hermes profile binds to the single user TraceDecay profile. First-party host bundles are versioned assets embedded in the trusted binary, with content digests and receipt-backed rollback but no custom signature/trust-root system or external bundle loading.
- The V2 roadmap treats PR6's daemon host-admission spool and PR16's remote offline-capture spool as distinct products with separate scope.
- Project-scoped host admission and ingestion must propagate an authoritative typed `ProjectId`; path aliases, symlinks, renames/moves, and mutable labels resolve through repository identity rather than minting split stores, while genuinely ambiguous claims fail closed and projectless Hermes uses user-profile authority. Source access derives from the daemon-authenticated route, exact `ResolvedScope`, and Plan 20 source binding/access rules; missing, stale, or ambiguous access is denied rather than unavailable, without synthetic grant or policy stores.
- TraceDecay V2 treats task/work graph and Kanban as a first-class product feature (Hermes-inspired, stronger), including persistent task/thread ownership, lossless retrieval, provider/model routing, performance-based task sizing and live recalibration, and optional local worktrees or stacked branches/PRs without requiring GitHub; it is not merely infrastructure for executing the V2 roadmap plans themselves. Plan 24 owns it semantically and PR17 delivers it as the first-class Work workspace plus task-graph projections, so PR14's dashboard scope is exactly the twelve existing workspaces (Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, Settings) and excludes Work; never substitute an independent Kanban store or session-derived tasks for the Plan 24 authority.
- PR13 GitHub/CI runtime uses existing `ureq` plus narrow typed Serde DTOs and one compile-time static GraphQL query; `gh api` is a manual fallback, while Octocrab, Backon, and GraphQL parser dependencies were rejected for this narrow runtime path.
- FastEmbed semantic indexing is asynchronous and must never block ordinary exact/lexical/graph retrieval: until a complete atomically current compatible vector generation exists, omit semantic results, preserve the unchanged PR9 result, and report indexing/degraded state; strict-semantic may return typed unavailable.
- The release/default feature posture includes all declared production features, including bundled-ORT FastEmbed; stable and beta workflows build, test, package, install, and run offline distribution acceptance with all features. FastEmbed defaults to the settings-selectable `JinaEmbeddingsV2BaseCode`, with offline-safe install and background acquisition/indexing surfaced through Doctor/status.
