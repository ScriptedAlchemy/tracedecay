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
- Invoke ordinary `cargo` commands. Zack's machine-local cargo shim/cargo-slot
  transparently allocates non-blocking lanes for concurrent Rust operations; do
  not bypass it, add repository lane coordination, or serialize Cargo work to
  avoid contention.
- Do not pause, kill, or disable Rust Analyzer to improve build timings. Its
  Claude Code LSP-owned processes are outside repository build optimization.
- The shim's local policy uses the checkout's repo-local `target/` by default
  and allocates isolated targets under `/fast/cargo-target/` when a non-blocking
  lane needs one. Let the shim select the lane; agents do not set
  `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` to manage contention.
- Cargo-launched TraceDecay test data follows the target selected by the shim.
  Never redirect targets or test data under `/tmp`, `$HOME`, or the root disk.
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
- Prefer GPT-5.6 Sol as the lead/orchestrator and Cursor Grok as scoped workers; delegate token-heavy evidence gathering while the lead independently verifies edits, synthesizes findings, and makes final judgments.
- When orchestrating parallel agents, the lead dictates exact scoped edits, subagents execute, and the lead reviews diffs before any push.
- Subagents should not invent scope beyond what the lead dictated.
- For Cursor plugin fixes, dogfood the official TraceDecay install or upgrade flow instead of hand-editing installed plugin files.
- In shared dirty checkouts with concurrent agents, work in-place (do not create worktrees), re-read files immediately before editing, and stage, commit, and push only changes made for the current task.
- For provider and observation acceptance, treat only checked-in real fixtures as binding evidence; reject synthetic, lookalike, or invented protocol fields.

## Git

- Every non-merge commit subject must pass `scripts/check-conventional-commits.sh` before push.
- Use `<type>: <subject>` or `<type>(<scope>): <subject>` with one of:
  `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Keep the subject at 72 characters or fewer. Example: `fix(doctor): avoid false orphan warnings`.

## Learned Workspace Facts

- Parallel branch work uses git worktrees under `.worktrees/` in the repo root (for example `.worktrees/codex-cli-args-stdin`).
- Integration/default branch is `master` (GitHub: ScriptedAlchemy/tracedecay).
- Cursor's TraceDecay plugin uses the MCP key `tracedecay`; Claude and Codex retain the `graph` key.
- Multi-PR merge verification: build a detached temporary worktree on
  `origin/master`, merge all target branches, then run ordinary Cargo tests and
  let the local shim allocate the isolated build and test-data lane.
- V2 Plan 35 assigns the daemon the LSP gateway/broker; Claude Code connects through one configured-language plugin, while non-LSP hosts receive equivalent diagnostics through hooks, hints, or MCP.
- V2 Plan 37 is the architectural center for branch-aware feedback cycles, read-only GitHub PR review-comment ingestion/surfacing (never posting, updating, resolving, or replying), and concurrent-agent proximity; LSP projects findings as editor evidence only and is not universal transport; it reuses existing diagnostic, graph, suggestion, workflow, and host-contract authorities.
- Plan 37's first coherent milestone spans PR11–PR13 and ships post-edit diagnostics and impact, CI failure localization, GitHub review ingest/display, and tiered agent proximity together; later PRs add dashboard/Doctor, multi-root, and remote composition without GitHub writes.
- V2 Plan 27 PR6 owns the host-neutral integration catalog model and observation adapters; PR13 owns packaging, registration, and lifecycle, and every Hermes profile binds to the single user TraceDecay profile.
- The V2 roadmap treats PR6's daemon host-admission spool and PR16's remote offline-capture spool as distinct products with separate scope.
- Project-scoped host admission and ingestion must propagate an authoritative typed `ProjectId`; paths and mutable labels are not identity sources, while projectless Hermes uses user-profile authority.
