# Production unwrap/panic triage

Scope: `src/upgrade.rs`, `src/agents/mod.rs`, `src/agents/claude.rs`, `src/global.rs`,
`src/sessions/source.rs` — the high-density files flagged by unsafe-pattern discovery —
plus a broad production sweep of `src/` to catch user-facing failure paths elsewhere.

Method: line-level review of every `.unwrap()` / `.expect(` / `panic!` / `todo!` /
`unimplemented!` / `unreachable!` match, classified as test-only or production.
Findings independently re-verified against the current tree (compile + targeted tests),
not from subagent self-reports.

## Result: all five target files are panic-free in production

Every unsafe-pattern occurrence in the five files sits inside a `#[cfg(test)]` module.
Production code paths use `?`, `Result`, `unwrap_or`, contextual logging, or graceful
early returns.

| File | Test-module boundary | Production region | Status |
|---|---|---|---|
| `src/upgrade.rs` | `#[cfg(test)] mod tests` @ 707 | 1–706 | clean — `install_binary` `?`-propagates, brew best-effort paths warn |
| `src/agents/mod.rs` | 6 inline `#[cfg(test)]` submodules (1024, 1541, 1711, 1792, 2113, 2135) | gaps between them | clean — all clusters (`migrate_tests`, `git_hook_tests`, `safe_config_tests`, `local_install_safety_tests`) are test-only |
| `src/agents/claude.rs` | `#[cfg(test)] mod tests` @ 1369 | 1–1368 | clean |
| `src/global.rs` | `#[cfg(test)]` @ 384 | 1–383 | clean — token-count read failure now emits contextual stderr + early return; future timestamps clamped |
| `src/sessions/source.rs` | `#[cfg(test)] mod tests` @ 577 | 1–576 | clean — missing/invalid inputs fail open with `tracing::debug!` context |

Verification: `cargo check --lib --bins` passes (0 new warnings); 91 tests pass across the
five files' modules (`upgrade::`, `sessions::source::`, `agents::{safe_config,local_install_safety,...}_tests`,
`gather_tests`), 0 failures.

## Broad production sweep (rest of `src/`)

After removing test code, the only production panic/expect/unreachable sites are all
intentional, documented, invariant-guarded fail-fasts — not fragile user-input unwraps:

| Site | Kind | Why it is acceptable |
|---|---|---|
| `src/mcp/server.rs:891` | `.expect("failed to register SIGTERM handler")` | Intentional fail-fast; a missing signal handler is an unrecoverable runtime misconfiguration. Documented. |
| `src/extraction/ts_provider.rs:51` | `panic!("unknown language key")` | Documented `# Panics`; keys come from extractors only, guarded by the `all_extractor_keys_are_registered` test. |
| `src/graph/scc.rs:89` | `unreachable!("work stack non-empty")` | Provably correct: guarded by the `let Some(frame) = work.last_mut() else { break }` on the prior line. Surrounding lookups already fail-open via `unwrap_or(&0)`. |
| `src/main.rs:1033` | `unreachable!("extract-worker handled by early dispatch")` | Documented clap-exhaustiveness arm; `ExtractWorker` short-circuits at the top of `run()`. |

No other production `unwrap()` / `expect(` / `panic!` was found outside test modules.

## Triage conclusion

- **User-facing failure paths (self-update / agent install): hardened.** The production
  code in `upgrade.rs`, `agents/mod.rs`, and `agents/claude.rs` no longer panics on
  unexpected input; errors propagate with context.
- **Transcript ingestion (`sessions/source.rs`): fail-open with debug context** for
  missing/malformed sources — appropriate for a background ingester.
- **Global token accounting (`global.rs`):** read failures are surfaced via stderr and
  skipped rather than silently treated as zero.
- **No follow-up hardening required.** The four remaining production panics are
  legitimate invariants, each documented or provably unreachable. They are listed here
  for traceability only; changing them would obscure real bugs (e.g. a missing grammar
  registration) rather than improve robustness.

This triage consolidates child cards `t_b2f43c97` / `t_5e0ff8e8` (upgrade.rs),
`t_e80f5ef0` (agents/claude.rs), `t_f7b13e24` (global.rs), `t_3516ea74`
(sessions/source.rs), and `t_d32fd5f3` (production validation), each independently
re-verified above.
