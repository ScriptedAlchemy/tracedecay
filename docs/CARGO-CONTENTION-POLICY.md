# Cargo target-dir contention policy for Kanban workers

**Status:** Active · **Owner:** `tracedecay` board · **Scope:** every Kanban worker on the `tracedecay` board that invokes `cargo` (`check` / `clippy` / `test` / `build` / `run`) from the terminal.

## The problem

All board workers share one working tree (`$HERMES_KANBAN_WORKSPACE` = the repo root). Cargo holds an **exclusive lock on its target directory for the entire duration of a build**. Any second `cargo` invocation pointed at the same target dir blocks with:

```
    Blocking waiting for file lock on build directory
```

That wait can last minutes and makes a perfectly healthy worker look frozen/stale to the dispatcher — missed heartbeats, `timed_out` reclaims, false `crashed` outcomes. With concurrent cards (the board routinely runs 5+ at once) and many of them refactor / review / test cards that compile, collisions are **guaranteed, not theoretical**.

Live evidence observed while authoring this policy: `target/debug/` and `target/.rustc_info.json` under the repo's default `target/` were rewritten mid-session by a worker running bare `cargo` — i.e. a worker was already building into the shared, contended dir.

## Why the MCP tools are not affected

The tracedecay MCP diagnostic tools (`tracedecay_diagnostics`, `tracedecay_run_affected_tests`, `tracedecay_affected`) already pin `--target-dir /tmp/tracedecay-target/<project-id>/diagnostics/` in `src/diagnostics/rust.rs` (`target_dir_for()`), deliberately so concurrent IDE / user `cargo check` runs don't race for `target/`'s lockfile and diagnostics never create repo-local TraceDecay folders. **Workers running their own `cargo` from the terminal are the only unmanaged case** — and they default to the repo's `target/`, which is the user's ~399 GB interactive dir.

## Policy

1. **Default: per-task isolated target dir.** Every cargo-heavy card exports, before its first `cargo` command:

   ```sh
   export CARGO_TARGET_DIR="/tmp/tracedecay-target/$HERMES_KANBAN_TASK"
   ```

   This is an independent cargo target root (cargo creates its own `debug/` / `release/` / `.cargo-lock` inside it). Different path ⇒ different lock ⇒ **zero contention** with other workers, with the MCP diagnostics target, or with the user's `target/`. Because the target lives under `/tmp`, it cannot pollute `git status` or create a project-local TraceDecay folder.

2. **Never run bare `cargo` against the repo's `target/`.** That dir is the user's interactive build cache. A worker building there contends with the human *and* with every other default-target worker. Always export `CARGO_TARGET_DIR` first.

3. **Leave `/tmp/tracedecay-target/<project-id>/diagnostics/` to the MCP tools.** Do not `cargo clean` it, do not point a worker at it. It is shared by `tracedecay_diagnostics` / `tracedecay_run_affected_tests`.

4. **Full-workspace integration uses its own target dir.** Integration checks follow the same per-invocation rule as other cargo-heavy cards. Use a unique target dir for the integration run so full-suite verification can run without blocking unrelated workers.

5. **Cleanup.** Per-task dirs are scratch. Before `kanban_complete`, a worker reclaims its disk (~1.6–4 GB each) with:

   ```sh
   rm -rf "$CARGO_TARGET_DIR"
   ```

   The owner periodically GCs dirs left behind by crashed / timed-out runs:

   ```sh
   ls /tmp/tracedecay-target/
   # remove entries whose task id is no longer running/ready/todo
   ```

6. **Docs / research / planning cards that never invoke `cargo` ignore this policy entirely.**

## Rejected alternatives

- **Serialize cargo cards on one shared target.** Rejected: it collapses the board to a single effective cargo lane, defeating the point of concurrent multi-model workers.
- **Set a repo-level `build.target-dir` in `.cargo/config.toml`.** Rejected: it would also hijack the user's interactive builds into one dir, re-creating contention with the human. The override must be per-invocation via the env var, not repo-wide.
- **Share compiled deps across per-task dirs.** Not natively supported by cargo without `sccache` / `cargo-chef`. Noted as a future optimization if cold-build time becomes a bottleneck; until then the bounded one-time compile per task is far cheaper than lock contention or serialization.

## Card-body snippet (owner: paste into every cargo-heavy card)

```text
Cargo policy (see docs/CARGO-CONTENTION-POLICY.md):
  export CARGO_TARGET_DIR="/tmp/tracedecay-target/$HERMES_KANBAN_TASK"
Run all cargo check/test/build/clippy AFTER that export. Never use the bare
repo `target/` (it is the user's interactive dir and is contended). Before
kanban_complete, reclaim disk:  rm -rf "$CARGO_TARGET_DIR"
Full-workspace integration checks use their own target dir too, so they do not
block unrelated workers.
```

## Verification checklist for a cargo-heavy card

- [ ] `CARGO_TARGET_DIR` exported before the first cargo call.
- [ ] No writes to the repo `target/` (the worker leaves `target/debug` mtime alone).
- [ ] `rm -rf "$CARGO_TARGET_DIR"` run before completion (unless intentionally reused by a chained follow-up card).
