---
name: dogfooding-tracedecay
description: Use when installing a TraceDecay checkout for live local development, refreshing the globally available binary without a release, or validating agents and the daemon against unmerged TraceDecay changes.
---

# Dogfooding TraceDecay

Use the repository command. Do not run release upgrade or point agents at
`target/`; Cargo may replace or lock that binary during later builds.

## Install the checkout

1. Confirm the checkout/branch contains the exact changes to dogfood and that
   unrelated worktrees are not being built accidentally.
2. From that checkout's root, run:

   ```bash
   cargo dogfood
   ```

3. Treat any nonzero exit as incomplete deployment. Inspect the printed stage,
   post-update, daemon, or doctor failure before retrying.

The command builds the ordinary development binary, copies it outside the repository to
`~/.local/lib/tracedecay/dogfood/tracedecay`, atomically replaces
`~/.local/bin/tracedecay`, refreshes tracked integrations through the normal
post-update lifecycle, restarts the managed daemon, and runs health checks.
`scripts/dogfood.sh` unsets `TRACEDECAY_DATA_DIR` and
`TRACEDECAY_DISABLE_GLOBAL_DB` before the live refresh, so post-update work
targets the real user profile rather than Cargo's isolated
`target/test-profile/.tracedecay`. That profile is left on disk, not deleted.

The development build runs `build.rs`, and `dashboard/app-dist/` is git-ignored, so
a fresh worktree has no bundle and the build shells out to `npm ci` plus
`npm run build` in `dashboard/`. Node.js 22+ and npm must be on PATH, and the
first dogfood in a new worktree pays that frontend build.

## Verify live use

Run these only after `cargo dogfood` succeeds:

```bash
command -v tracedecay
tracedecay --version
tracedecay daemon status
tracedecay doctor
```

A checkout build names the commit it was compiled from, as SemVer build
metadata appended to the released version:

```
tracedecay 0.0.66+330e47a0e780          # built from that commit, clean tree
tracedecay 0.0.66+330e47a0e780.dirty    # built with uncommitted changes
```

Compare that commit against the checkout you meant to deploy (`git rev-parse
--short=12 HEAD`); a mismatch means an older binary is still installed. A
`.dirty` build corresponds to no commit at all, which is ordinary while
iterating but is not something a verification claim should rest on. The suffix
is build metadata, ignored for SemVer precedence, so release-plz still owns the
bare `0.0.66` — never hand-edit `version` in `Cargo.toml` to mark a dogfood
build. A binary installed from a published release has no commit to name and
prints `tracedecay 0.0.66`.

`tracedecay doctor`, `tracedecay daemon status`, the MCP `serverInfo`
handshake, and the dashboard all report that same string, so a daemon left
running from an earlier dogfood build now shows as a version mismatch instead
of looking current.

Confirm the upgrade actually took effect on the live profile: exactly one
managed daemon process is running (`systemctl --user show tracedecay.service
-p MainPID`), doctor's current-project integrity check passes under the new
daemon owner, and a daemon-brokered call answers (for example
`tracedecay tool memory_status --args '{}'`). Schema migrations run when the
new daemon first opens each store — roll forward only; never let an older
daemon reopen a store a newer binary has migrated.

Then reproduce the changed host scenario with the ordinary global
`tracedecay` command. Inspect relevant service/host logs. Restart a host only
when its integration is in-process or its plugin module cannot hot-reload.

## Boundary recovery

`cargo dogfood` crosses a forward-only migration boundary once the new
binary replaces the installed one; after that point it will never restore
or execute an older binary, only recover forward. If a run dies mid-way
(the marker at `~/.tracedecay/dogfood-migration-boundary.state` reads
`forward-recovery-required`), fix or rebuild a schema-compatible newer
binary and rerun `cargo dogfood` — do not touch any prior binary against
the live stores. For the marker format, the `TRACEDECAY_DOGFOOD_BACKUP` /
`TRACEDECAY_DOGFOOD_BACKUP_PLAIN` backup contract, and the recovery ladder
(zero-writer proof → restore/backup → `tracedecay init` rebuilds the
derived registry from on-disk enrollment markers → rerun `cargo dogfood`),
see `docs/DOGFOOD-RECOVERY-RUNBOOK.md`.

## Verify the live daemon end-to-end

The manual commands above confirm the binary. To confirm the *running daemon*
behind it, use the live suite:

```bash
scripts/live-daemon-check.sh
```

It builds and runs `tests/live_daemon_suite.rs` against the real managed daemon
and finishes with a PASS/FAIL table covering:

- socket connect plus an MCP `initialize` whose `serverInfo.version` must equal
  the installed binary's `--version` — the stale-daemon check;
- `tools/list` sanity (full catalog, no duplicates, the always-loaded tools
  present);
- a daemon-brokered read battery (`status`, `search`, `grep`, `callers` on the
  node the search resolved, `storage_status`, `git_status`);
- latency bounds (every call under 30s, `status` under 5s);
- a `tracedecay doctor` pass, which exits zero when it counted zero issues
  regardless of warnings;
- the daemon still being the same process, with the socket still accepting,
  after the battery.

The suite is read-only: it handshakes with `allow_init = false`, dispatches no
writes, and never starts, stops, restarts, or signals the daemon. It excludes
`tracedecay_memory_status`, whose handler repairs derived holographic vectors;
set `TRACEDECAY_LIVE_DAEMON_ALLOW_MEMORY_STATUS=1` to include it.

Both guards must be satisfied for anything to run — every test is `#[ignore]`
*and* gated on `TRACEDECAY_LIVE_DAEMON_TESTS=1` — so a plain `cargo test` and
CI execute zero of them. Useful overrides: `TRACEDECAY_BIN` (binary to
cross-check), `TRACEDECAY_LIVE_DAEMON_PROJECT` (indexed project to route to;
must already be indexed, the suite refuses to create one),
`TRACEDECAY_LIVE_DAEMON_SYMBOL` / `TRACEDECAY_LIVE_DAEMON_PATTERN` (probe
inputs when running against a checkout other than TraceDecay itself).

## Guardrails

- Invoke ordinary `cargo dogfood` without setting `CARGO_TARGET_DIR` or
  `TRACEDECAY_DATA_DIR`: a redirected target dir breaks the staged-binary path,
  and an injected data dir deploys against an isolated test profile instead of
  the real one.
- Do not run `tracedecay upgrade` for checkout dogfood; it installs a published
  release.
- Do not kill host-owned MCP shim processes indiscriminately.
- Do not delete the source worktree until its commits are pushed and merged.
- Re-run `cargo dogfood` after any source change that must reach live agents.
