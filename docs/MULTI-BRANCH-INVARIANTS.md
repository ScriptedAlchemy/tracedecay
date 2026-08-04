# Multi-Branch Indexing — Invariant Map

Audit of the tracedecay multi-branch indexing behavior: state transitions,
persistence locations, fallback rules, and stale-state risks.

Scope: the indexed (code-graph) multi-branch subsystem, not the session/LCM
compression subsystem. Complements [MULTI-BRANCH-DESIGN.md](MULTI-BRANCH-DESIGN.md)
(the *why/architecture*) and [BRANCHING-USER-GUIDE.md](BRANCHING-USER-GUIDE.md)
(the *how-to*); this doc is the actionable *invariants + risks* reference.
For hands-on diagnosis and recovery when state has already drifted, see
[MULTI-BRANCH-RECOVERY.md](MULTI-BRANCH-RECOVERY.md).

All `file:line` references are against the current tree.

---

## 1. Persistence locations

| Artifact | Path | Owner | Notes |
|---|---|---|---|
| Project data dir | `<root>/.tracedecay/` (legacy `<root>/.tracedecay/`) | `config::get_tracedecay_dir` (`config.rs:87`) | Prefer `.tracedecay`; fall back to an *existing* legacy dir read+write. Brand-aware everywhere via `db_filename` (`config.rs:115`). |
| Default-branch DB | `<data_dir>/tracedecay.db` (legacy `tracedecay.db`) | `init`/`resolve_db_for_branch` | `DB_FILENAME` for new dirs; `LEGACY_DB_FILENAME` inside legacy dirs. |
| Branch DBs | `<data_dir>/branches/<stem>.db` (+ `.db-wal`/`.db-shm`) | `branch_meta::ensure_branches_dir` (`branch_meta.rs:148`), created in `add_branch_tracking`/CLI `branch add` | One SQLite file per non-default tracked branch. |
| Branch metadata | `<data_dir>/branch-meta.json` | `branch_meta::{load,save}_branch_meta` (`branch_meta.rs:125,141`) | Source of truth for `default_branch`, tracked branches, `db_file`, `parent`, timestamps. |
| In-memory state | `TraceDecay { active_branch, serving_branch, fallback_warning }` | `tracedecay.rs` (struct ~line 298-306) | Resolved at `open()` time; see §3. |

No branch data is stored in git. Branch DBs are plain files on disk keyed by
`branch-meta.json`; there is no `branch` column in any graph table.

---

## 2. DB-stem → file mapping (the collision invariant)

**Invariant:** each distinct branch name must map to a *distinct* `.db` file,
and that mapping must be stable (the same branch always re-resolves to the same
file).

The branch-tracking path is centralized through
`TraceDecay::add_branch_tracking`, which keeps sync orchestration in the engine
and branch metadata/copy work in `branch.rs`.

- **Branch metadata path** (`branch::prepare_branch_tracking_in_layout`): uses
  `unique_branch_db_stem`, which returns the bare sanitized stem only when it is
  free, otherwise appends a short content hash of the *unsanitized* name. This
  guards against `sanitize_branch_name` being many-to-one (`feature/foo` ≡
  `feature_foo`) and against re-`fs::copy`-overwriting an existing branch index.
- **CLI/hook path** (`TraceDecay::add_branch_tracking`): calls the
  same preparation path, so CLI `branch add`, Cursor/Codex hooks, and Cursor
  install branch tracking share DB-stem collision behavior.

**Empty-name guard:** both paths reject a name that sanitizes to empty
(library: `branch.rs:274,298`; would otherwise produce a hidden
`branches/.db`).

**Path-traversal guard:** `resolve_branch_db_path` (`branch.rs:154`) checks the
resolved path stays inside the data dir — but only when *both* canonicalize()
calls succeed (`branch.rs:162-168`). For an already-existing tracked branch the
stem is sanitized so this is safe; for a hand-edited meta `db_file` containing
`..` that points at a non-existent target, canonicalize fails and the
containment check is skipped (returns the path unchecked). Trust boundary is the
local meta file, so low severity, but untested.

---

## 3. Active vs. serving branch state

`TraceDecay` carries three fields resolved at `open()` time:

- `active_branch` — the live git branch at open (`branch::current_branch`,
  `branch.rs:12`; gix first, `git symbolic-ref HEAD` fallback for worktrees).
  `None` ⇒ detached HEAD or not a repo.
- `serving_branch` — the branch whose DB is *actually* open. `None` ⇒
  single-DB mode (no `branch-meta.json` / non-git project).
- `fallback_warning` — `Some` ⇔ `active ≠ serving` (serving an ancestor/default
  DB because the active branch is untracked).

### 3a. DB resolution cascade — `resolve_db_for_branch` (`tracedecay.rs:433`)

Priority order when opening for a branch:

1. No `branch-meta.json` → **single-DB mode**: default DB,
   `serving=None`, no warning. (`tracedecay.rs:440`)
2. Detached HEAD (`active=None`) → default-branch DB,
   `serving=default`, "detached HEAD" warning. (`tracedecay.rs:445`)
3. Branch tracked **and** its `.db` exists → that branch's DB,
   `serving=branch`, no warning. (`tracedecay.rs:455`) — the only non-fallback case.
4. Nearest tracked ancestor (via `git merge-base`) **and** its DB exists →
   ancestor DB, `serving=ancestor`, fallback warning naming the branch to
   `add`. (`tracedecay.rs:462`)
5. Last resort → default-branch DB, `serving=default`, fallback warning.
   (`tracedecay.rs:477`)

**Invariant:** reads *always* succeed (they fall through to *some* DB); only
writes are gated (§4). This is deliberate — serving a possibly-stale ancestor
DB is preferable to erroring the agent's read.

### 3b. Ancestor seeding — `find_nearest_tracked_ancestor` (`branch.rs:176`)

Iterates tracked branches, computes `git merge-base` with the target, picks the
**most recent** common ancestor (by commit time).

> ⚠️ **Risk B — silent default-seed when the branch ref is unresolvable.**
> `find_nearest_tracked_ancestor` requires the *target* branch ref to peel to a
> commit via gix; if gix can't see it (just-created ref,
> worktree-local ref, gix ref-store not refreshed), it returns `None` and both
> branch tracking entry points fall
> back to seeding from `default_branch`. Result: a branch whose files are
> closest to a non-default ancestor gets seeded from `main`/`master` instead —
> a larger initial sync and a worse first-query experience, not data loss.
> Untested for the "fresh ref not visible to gix" case.

---

## 4. Write gating — `ensure_branch_writable` (`tracedecay.rs:2894`)

Every mutating path (`index_all` `:798`, `sync` `:975`/`:1012`/`:1052`/`:1293`,
and the stale-sync variants) calls `ensure_branch_writable` first. It refuses
the write with a `Config` error in two cases:

1. **Fallback case** (`is_fallback()`): active branch is served from an
   ancestor/default DB. Message tells the user to `tracedecay branch add
   <active>`. Prevents indexing an untracked branch's files into the wrong DB.
2. **Drift case** (`tracedecay.rs:2918`): the live git branch no longer matches
   `serving_branch`. Prevents a long-lived instance (pinned at open time) from
   writing the new branch's files into the old branch's DB. Single-DB mode
   (`serving=None`) is explicitly exempt.

Read paths do **not** call this — they can serve stale data silently (with the
warnings surfaced by `tracedecay_status`, see §5).

### 4a. Branch drift + hot reopen (MCP server)

`branch_drifted()` (`tracedecay.rs:2947`) compares the *live* branch to
`active_branch` (open-time), **not** `serving_branch` — so reopening clears the
drift even when the new branch is untracked and legitimately falls back,
avoiding a reopen loop.

The MCP server wraps the served `TraceDecay` in an `RwLock<Arc<…>>` and, on
every `tools/call`, runs `reopen_if_branch_drifted` (`mcp/server.rs:491`):
fast-path gix HEAD check; on drift, a write lock re-checks and swaps in a fresh
`open()` at most once. After a swap it refreshes the file-token map
(`:522`). On reopen failure it keeps the old instance (the write guards in §4
still protect). `maybe_sync_if_stale` (`mcp/server.rs:701`) also early-returns
when `branch_drifted()` to avoid diffing the new branch's files against the old
DB (`:731`).

**Invariant:** the served instance always reflects the live branch *or* an
explicitly-warned fallback; a stale write is structurally impossible because
both the lazy-sync and the explicit-sync paths re-check drift/writability.

---

## 5. Cross-branch tools (MCP)

All in `src/mcp/tools/handlers/git.rs`, read-only, each opens its own
`TraceDecay::open_branch` (`tracedecay.rs:493`) for the target branch:

- **`branch_list`** (`git.rs:846`) — enumerates `branch-meta.json`; reports
  `is_current` (vs `cg.active_branch()`), `is_default`, `size_bytes`,
  `last_synced_at`. Reads meta only, never opens branch DBs.
- **`branch_search`** (`git.rs:887`) — `open_branch(branch)` then `search`.
  Errors if branch untracked or DB missing.
- **`branch_diff`** (`git.rs:938`) — resolves `base` (default branch if
  omitted) and `head` (active branch if omitted). Opens both via
  `open_branch`; **optimization** (`git.rs:987`): reuses the already-open live
  `cg` for head only when `active_branch()==head && !is_fallback()`. Same-ref
  short-circuits to an empty diff (`git.rs:962`).

`tracedecay_status` (`info.rs:29-61`) surfaces `active_branch`,
`current_branch`/`live_branch`, `serving_branch`, `parent_branch`,
`branch_drifted`, `branch_resolution`, `serving_db_path`/`serving_db_exists`,
and the full `branch_diagnostics` object. It also adds a `branch_mismatch`
block when the live checkout diverges from the open-time active branch, and
`branch_fallback`/`branch_warning` when serving an ancestor/default DB.

---

## 6. Lifecycle operations (CLI `tracedecay branch …`)

Handler: `commands::handle_branch_action` (`commands.rs:66`). `BranchAction`
variants: `List | Add | Remove | Removeall | Gc`.

| Op | Behavior | Meta write |
|---|---|---|
| `list` (`:73`) | Print default + tracked branches; `*` marks current; shows size/parent/synced. | read-only |
| `add` | Detect-or-arg branch; delegate through `TraceDecay::add_branch_tracking`; bootstrap meta if absent; seed from nearest ancestor (or default); `fs::copy` parent DB → collision-safe `branches/<stem>.db`; save meta; open+sync; `touch_synced`. | write |
| `remove` (`:236`) | Refuse on default branch; else `meta.remove_branch`, `remove_file` (+WAL/SHM), save. | write |
| `removeall` (`:262`) | Remove every non-default branch + its DB sidecars; save. | write |
| `gc` (`:290`) | Detect branches in meta whose ref no longer exists in git; remove them + DBs; save. | write |

> ⚠️ **Risk C — `branch gc` ref detection is filesystem-heuristic, not gix.**
> It checks `.git/refs/heads/<name>` and a `ends_with("refs/heads/<name>")`
> scan of `.git/packed-refs` (`commands.rs:303-312`). The suffix match is
> delimiter-safe (the `refs/heads/` prefix prevents `dev` matching
> `release-dev`), but it assumes a standard `.git` layout: **linked worktrees
> and bare repos** resolve refs differently and could cause a still-existing
> branch to be classified stale and deleted (DB destroyed). The rest of the
> subsystem uses gix; this one command shells out to raw paths. Untested for
> worktree/bare layouts.

Auto-tracking entry points (library, safe path): Cursor `afterShellExecution`
classifies the command into `CursorShellSyncPlan` (`hooks.rs:567`) —
`BranchAdd` for detected branch switches, `CurrentBranchSync` for state-changing
commands with a known current branch, else `IncrementalSync`/`Noop`. Cursor
`workspaceOpen` calls `workspace_open_for_cursor_event` (`hooks.rs:1119`) which
`add_branch_tracking`s the current branch (and skips the catch-up sync since
add already syncs). Codex has a parallel path (`hooks.rs:1376`).

---

## 7. State-transition summary

```
init()                         open()/reopen()                add (library)
──────                         ──────────────                 ────────────
create data dir                active = current_branch         already tracked? → AlreadyTracked (noop)
create default DB              resolve cascade §3a →           ancestor = merge-base (or default)
write branch-meta.json            (db_path, serving, warn)      fail-fast on empty stem
active = current_branch        open DB (crash→rebuild)          stem = unique_branch_db_stem (collision-safe)
serving = None                 if dirty sentinel: rebuild      fs::copy ancestor.db → branches/<stem>.db
                               + re-index                      save meta (db_file=branches/<stem>.db)
                                                                open() (resolves new branch) + sync()
                                                                touch_synced

write (sync/index)             drift (MCP tools/call)          remove/gc
──────────────────             ──────────────────              ──────────
ensure_branch_writable:        branch_drifted()?               remove_branch (refuse default)
  fallback?  → REJECT            → reopen_if_branch_drifted    unlink DB + WAL/SHM
  live≠serving? → REJECT         (swap served instance)         save meta
else write to serving DB        else use snapshot
```

---

## 8. Fallback rules (recap)

1. **No meta** → single-DB mode (default DB); drift/writability guards exempt.
2. **Detached HEAD** → default DB + warning.
3. **Untracked branch** → nearest-ancestor DB if it exists, else default DB;
   both with a "run `tracedecay branch add`" warning; **writes refused**.
4. **Corrupt `branch-meta.json`** → warning + `None` ⇒ silent single-DB mode.
5. **Crash mid-sync** (dirty sentinel) → integrity-check, rebuild if corrupt.
6. **Corrupt/unopenable DB** → delete + re-initialize + re-index.

---

## 9. Known stale-state risks & recommended follow-up tests

Implemented invariants now have explicit diagnostics/test coverage; risks left
open below are intentional documented gaps rather than silent omissions.

| Invariant / finding | Diagnostic surface | Test coverage | Remaining gap |
|---|---|---|---|
| Open-time branch drift must refuse writes and reopen cleanly | `BranchDiagnostics.branch_drifted`, `branch_resolution=stale_serving_branch`, status `branch_mismatch` | `tests/storage_suite/branch_drift_test.rs::sync_refuses_to_write_after_mid_session_branch_checkout`; MCP hot-reopen regressions in `tests/mcp_server_test.rs` | None known for the library/MCP path. |
| Untracked live branches serve an explicit ancestor/default fallback and refuse writes | `is_fallback`, `fallback_target`, `nearest_tracked_ancestor`, `branch_resolution=fallback_ancestor|fallback_default` | `tests/storage_suite/branch_drift_test.rs::branch_diagnostics_reports_fallback_target_and_nearest_ancestor` | Fresh-ref/gix visibility edge remains Risk B. |
| Tracked branch metadata with a missing DB is visible to operators | `live_branch_db_exists=false`, per-branch `db_exists=false`, `[missing-db]`, warning strings | `tests/storage_suite/branch_drift_test.rs::branch_diagnostics_flags_missing_tracked_branch_db` | Recovery is documented; no automated doctor repair yet (Risk D). |
| MCP and CLI expose live/open/serving state consistently | `tracedecay_branch_list`, `tracedecay_status`, `tracedecay://status`, CLI `tracedecay branch list` | `tests/mcp_handler_test.rs::test_branch_list_reports_live_vs_serving_drift_state`; `tests/mcp_handler_test.rs::test_status`; `tests/storage_suite/branch_drift_test.rs` diagnostics assertions | CLI branch lifecycle E2E coverage remains Risk F. |

| # | Risk | Severity | Where | Suggested test |
|---|---|---|---|---|
| B | Fresh branch ref invisible to gix silently seeds from default | Med | `branch.rs` ancestor lookup, `TraceDecay::add_branch_tracking` callers | Unit/integration: create branch, force gix ref-store lag (or mock), assert seeding source and that the warning/retry path is correct. |
| C | `branch gc` filesystem-heuristic ref detection mis-handles worktrees/bare | Med (destructive) | `commands.rs:303-312` | Integration test in a linked worktree and a bare repo: assert `gc` does not delete a still-existing branch. Consider reimplementing on gix. |
| D | Corrupt `branch-meta.json` → silent single-DB mode leaves orphaned `branches/*.db` | Low | `branch_meta.rs:130`, `resolve_db_for_branch` | `gc`/a new `doctor` check should detect `branches/*.db` not referenced by any meta entry. |
| E | Path-traversal guard skipped when target doesn't exist | Low | `branch.rs:162-168` | Unit test with a crafted meta `db_file` containing `..` pointing at a missing path; assert it's rejected, not returned unchecked. |
| F | No end-to-end coverage of the CLI `branch add/remove/gc` commands | Process | `commands.rs` (all variants) | The branch test suite (`branch_db_safety_test.rs`, `branch_drift_test.rs`) mostly drives the library `TraceDecay` API; add CLI-binary tests for each `BranchAction`. |

Items A, C, F are the highest-value follow-ups: A is a concrete data-loss bug
masked by a misleading doc comment; C is destructive on non-standard layouts;
F is why A and C went unnoticed.

---

*Audit performed against the current `master` tree. Re-verify `file:line`
references after merges; the invariants themselves are stable.*
