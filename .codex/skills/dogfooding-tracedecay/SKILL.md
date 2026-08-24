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

The command builds the release binary, copies it outside the repository to
`~/.local/lib/tracedecay/dogfood/tracedecay`, atomically replaces
`~/.local/bin/tracedecay`, refreshes tracked integrations through the normal
post-update lifecycle, restarts the managed daemon, and runs health checks.
Cargo's isolated `target/test-profile/.tracedecay` profile is removed before
the live refresh.

## Verify live use

Run these only after `cargo dogfood` succeeds:

```bash
command -v tracedecay
tracedecay --version
tracedecay daemon status
tracedecay doctor
```

Then reproduce the changed host scenario with the ordinary global
`tracedecay` command. Inspect relevant service/host logs. Restart a host only
when its integration is in-process or its plugin module cannot hot-reload.

## Guardrails

- Do not run `tracedecay upgrade` for checkout dogfood; it installs a published
  release.
- Do not kill host-owned MCP shim processes indiscriminately.
- Do not delete the source worktree until its commits are pushed and merged.
- Re-run `cargo dogfood` after any source change that must reach live agents.
