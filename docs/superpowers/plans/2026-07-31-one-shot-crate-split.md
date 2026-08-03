# One-Shot Crate Split (owner decision 2026-07-31)

Supersedes the phased breakup plans (deleted). Owner rulings: **no phases** —
one mass move of all root subsystems into workspace crates; **breakage during
the move is acceptable** ("move all crate code first, then deal with the
aftermath once the builds will no longer be slow"); validation is the whole
product end to end, not per-move gates.

## Why

`src/` is ~700K lines in one crate — every edit recompiles a 1.3 GB rlib that
~56 test binaries relink. Measured duplication with the 21 existing crates is
~0.9%; the mass is genuinely unsplit subsystems. Only moving them out shrinks
the serial build tail.

## Target map (one landing)

| From src/ | To crate | Notes |
|---|---|---|
| semantic_code | tracedecay-semantic | DONE (landed) |
| search_eval | tracedecay-search-eval (new) | in flight; bins stay root |
| mcp/core_hooks.rs | tracedecay-hooks | in flight (de-knot) |
| mcp/transport.rs | tracedecay-jsonrpc (new) | in flight (de-knot) |
| mcp CodeIndexSearch types | tracedecay-query | in flight (de-knot) |
| DaemonInvocation* types | tracedecay-application | in flight (de-knot) + ratchet guard |
| sessions/ | tracedecay-sessions (exists, façade) | mover assigned |
| migrate/ | tracedecay-migrate (exists) | mover assigned |
| global_db/ | tracedecay-global-db (new) | mover assigned |
| agents/ + automation/ | tracedecay-agent-hosts (new) | move together (mutual recursion); embedded assets need crate-local build.rs/paths |
| dashboard/ (minus assets.rs) | tracedecay-dashboard-api (new) | assets.rs stays (OUT_DIR embed) |
| errors, types, timeutil, storage, db, store, memory, sync, git, worktree, branch_meta, lifecycle_lease, sqlite_read_snapshot, path_scope, privacy, redundancy, runtime_identity, serde_util, text, os_str_bytes, windows_file, open_store_holders | tracedecay-runtime-core (new) | kernel; verifier report refines composition |
| mcp/ (rest) | tracedecay-mcp (new) | after de-knot lands |
| daemon/ + daemon.rs | tracedecay-daemon (new) | after de-knot lands |
| application/ (rest) + application_surface | tracedecay-application / tracedecay-api | after de-knot lands |

Root keeps: main.rs, cli*, commands/, *_cmd.rs adapters, bin/, config,
dashboard/assets.rs, hooks/ daemon-side handlers (cycle with daemon), and thin
`pub use` shims for every moved path so tests/ imports survive.

## Execution model

- One mover agent per subsystem in an isolated worktree; `git mv` whole
  modules; shim files at old paths; each mover compiles only ITS crate
  (best-effort) — a red root is acceptable and expected mid-landing.
- Lead octopus-merges all mover branches into the split landing, resolves
  Cargo.toml/lock unions, then runs the single mass fix-to-green campaign
  (fleet of fixer agents on compile errors, then whole-product validation:
  build, isolated release validation, CI).
- Cycle edges break by moving shared pure-data types DOWN (never by adding
  upward deps). The architecture ratchet tests (compile_isolation + the new
  mcp/daemon direction guards) are the only per-move gates that must stay
  green at the END of the landing.

## Aftermath queue (fix after the move)

Compile errors from split seams (each mover's SEAMS.md is the work order);
feature forwarding (production, test-transport, semantic-fastembed,
lite/full); embedded asset paths (include_bytes/include_str) in agent-hosts;
scripts that name old paths (check-distribution-acceptance.sh); doc
references. All adjudicated by the whole-product gate, not per-crate
ceremony.

### Test relocation and import taming (owner order 2026-07-31)

- **Tests move with their subjects.** Unit tests live in the crate that owns
  the code; root integration tests that exercise one crate's surface migrate
  into that crate's tests/ (with their fixtures, fixing the cargo-package
  escapes the movers cataloged). The ~56 root test binaries shrink to the
  cross-crate journeys only — that is where the relink win lives.
- **Tame the imports.** The root shims (`pub use tracedecay_x::*`) are
  transitional scaffolding, not architecture. After green, ast-grep sweeps
  repoint root and test call sites from shim paths (`crate::sessions::X`)
  to direct crate imports (`tracedecay_sessions::X`), then the glob shims
  shrink and die. No permanent double-path for the same item.
- **Scar cleanup once settled.** Delete each SEAMS.md as its rows resolve;
  re-seal the benchmark manifests whose digests pinned old harness paths;
  update docs citing src/ paths; audit the mass pub-widenings (613 sessions,
  491 dashboard, 15 agent-hosts) and narrow anything no external caller
  names; remove the `// SEAM(...)` markers; drop root Cargo.toml include
  entries that moved with their crates.
