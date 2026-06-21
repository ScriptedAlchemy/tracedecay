# Multi-Branch Indexing — Recovery Runbook

Operator-facing playbook for diagnosing and recovering from multi-branch
indexing drift. Covers how to inspect current active/serving branch state, how
to read the diagnostics added in the branch-drift work, how to rebuild or copy
branch DBs safely, when to reset serving-branch fallback, and what to capture
before any destructive recovery.

Scope: the indexed (code-graph) multi-branch subsystem, not the session/LCM
compression subsystem.

> **Read this first — the three docs together:**
> - [MULTI-BRANCH-DESIGN.md](MULTI-BRANCH-DESIGN.md) — the *why / architecture*
> - [BRANCHING-USER-GUIDE.md](BRANCHING-USER-GUIDE.md) — the *how-to* (day-to-day use)
> - [MULTI-BRANCH-INVARIANTS.md](MULTI-BRANCH-INVARIANTS.md) — the *invariants + risks* reference
> - **This doc** — the *diagnosis + recovery* runbook (when something is already wrong)

Every command below is marked **[read-only]** (safe to run any time, no state
changes) or **[mutating]** (writes/deletes DB files or metadata — capture first,
see §1). All `file:line` references are against the current tree; re-verify
after merges.

---

## Where multi-branch state lives

| Artifact | Path | Notes |
|---|---|---|
| Data dir | `<root>/.tracedecay/` (legacy `<root>/.tracedecay/`) | Brand-aware; legacy dir honored as fallback if it already exists. |
| Default-branch DB | `<data_dir>/tracedecay.db` (+ `.db-wal`/`.db-shm`) | The DB for `main`/`master`. |
| Branch DBs | `<data_dir>/branches/<stem>.db` (+ sidecars) | One per non-default tracked branch. |
| Branch metadata | `<data_dir>/branch-meta.json` | Source of truth: `default_branch`, tracked branches, `db_file`, `parent`, timestamps. |
| In-memory (running instance) | `TraceDecay { active_branch, serving_branch, fallback_warning }` | Resolved at `open()` time (see §6). |

No branch data is stored in git. Branch DBs are plain files keyed by
`branch-meta.json`.

---

## §1. Capture before destructive recovery **[read-only]**

Before running **any** `[mutating]` command (§4–§5), snapshot enough state to
reconstruct what went wrong and to roll back. Keep these copies until recovery
is verified (§7).

```bash
# 1. Record the current diagnosis (human-readable + raw JSON from the MCP server).
tracedecay branch list                  > drift-before.txt 2>&1
tracedecay status --short              >> drift-before.txt 2>&1

# 2. Snapshot metadata and every branch DB (including WAL/SHM sidecars).
DATA_DIR="$(git rev-parse --show-toplevel)/.tracedecay"   # or .tracedecay on legacy projects
BACKUP="$DATA_DIR/../.tracedecay-recovery-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"
cp -a "$DATA_DIR/branch-meta.json" "$BACKUP/" 2>/dev/null
cp -a "$DATA_DIR"/branches/*.db      "$BACKUP/" 2>/dev/null
cp -a "$DATA_DIR"/branches/*.db-wal  "$BACKUP/" 2>/dev/null
cp -a "$DATA_DIR"/branches/*.db-shm  "$BACKUP/" 2>/dev/null
cp -a "$DATA_DIR/tracedecay.db"       "$BACKUP/default-tracedecay.db" 2>/dev/null

# 3. Record git state (which refs exist — drives `branch gc` decisions).
git rev-parse --abbrev-ref HEAD       > "$BACKUP/git-head.txt"
git for-each-ref refs/heads           > "$BACKUP/git-refs.txt"

# 4. Capture the MCP server log if you have one (path varies by agent).
ls -t ~/.hermes/profiles/*/logs/*.log 2>/dev/null | head -1 | xargs -r tail -n 500 > "$BACKUP/mcp-tail.txt"
```

> ⚠️ Never run `[mutating]` recovery against a DB the MCP server still has open
> with an active WAL. Copying or deleting a `.db` without its `.db-wal`/`.db-shm`
> sidecars can lose or roll back committed index pages. Stop the server first
> (restart the agent / kill `tracedecay mcp`), or rely on the CLI commands which
> open short-lived instances.

---

## §2. Diagnose **[read-only]**

Run the diagnosis windows below to figure out *which* branch state is wrong.
The key distinction is **live branch** (what git says now) vs **open-time active
branch** (what the running instance pinned at startup) vs **serving branch**
(whose DB is actually answering queries).

### 2a. CLI — `tracedecay branch list` (best human-readable view)

```bash
tracedecay branch list           # [read-only]  uses current path
tracedecay branch list -p ../other-worktree
```

Sample output:

```
Default branch: main
Current branch: feature/parser
Serving branch: main (fallback)
Opened branch: main

  main [default, serving] — 206.3 MB, synced 5m ago
  feature/foo — 207.1 MB (from main), synced 2h 10m ago
  release/3.4 [current] — 205.8 MB (from main), synced 1d ago
  hotfix/9 [missing-db] — missing (from main), synced 3d ago

warning: branch drift detected: working tree is on 'release/3.4' but this instance
opened on 'main' and is still serving 'main'. Reopen the index so reads and
writes target the live branch.
warning: tracked branch 'hotfix/9' is listed in branch metadata but its DB is
missing at '.tracedecay/branches/hotfix_9.db'; serving 'main' instead.
```

How to read it:

| Line / flag | Meaning |
|---|---|
| `Default branch` | `branch-meta.json` → `default_branch` (the last-resort DB). |
| `Current branch` | Live git HEAD (`branch::current_branch`). `<detached HEAD>` if not on a ref. |
| `Serving branch` | Whose DB is open right now. `(fallback)` ⇔ serving an ancestor/default, not the live branch. |
| `Opened branch` | Only printed when drifted: the branch the running instance opened on (≠ live). |
| `*`/`[current]` | This branch matches live git HEAD. |
| `[serving]` | Whose DB is answering queries. |
| `[missing-db]` | Tracked in meta but `.db` is gone on disk — needs re-seed (§4b). |
| `warning:` lines | Machine-readable problems; each names the recovery action. |

### 2b. CLI — `tracedecay status`

```bash
tracedecay status --short       # [read-only]
```

The header prints `branch:` (serving), `from:` (parent), and a `⚠ fallback`
marker. **Note:** `tracedecay status --json` only serializes `GraphStats` — it
does **not** include branch diagnostics. For structured branch state use
`tracedecay branch list` (human) or the MCP surfaces below (JSON).

### 2c. MCP tools and resources (full structured diagnostics)

```jsonc
// tool: tracedecay_status   [read-only]
// resource: tracedecay://status
{
  "active_branch": "main",          // open-time pinned branch
  "current_branch": "feature/parser", // live git HEAD ("live_branch" alias also set)
  "serving_branch": "main",
  "branch_drifted": true,
  "branch_resolution": "stale_serving_branch",
  "tracked_branch_count": 3,
  "serving_db_path": ".tracedecay/tracedecay.db",
  "serving_db_exists": true,
  "branch_warnings": [ "branch drift detected: ..." ],
  "branch_diagnostics": {
    "is_fallback": false,
    /* full BranchDiagnostics object, see §8 */
  }
}
```

```jsonc
// tool: tracedecay_branch_list   [read-only]
// resource: tracedecay://branches
{
  "tracking_enabled": true,
  "default_branch": "main",
  "current_branch": "feature/parser",
  "serving_branch": "main",
  "branch_resolution": "fallback_ancestor",
  "is_fallback": true,
  "fallback_target": "main",
  "nearest_tracked_ancestor": "main",
  "live_branch_tracked": false,
  "live_branch_db_exists": null,
  "branches": [
    { "name": "main", "is_default": true, "is_serving": true, "db_exists": true, "size_bytes": 216261120, "warnings": [] },
    { "name": "hotfix/9", "db_exists": false, "warnings": ["missing DB at '...'"] }
  ]
}
```

The full `branch_resolution` vocabulary and every field are tabulated in §8.

---

## §3. Decision matrix — symptom → action

Match what §2 showed to the right recovery. **Always diagnose (§2) and capture
(§1) before applying the action in the right column.**

| Symptom (from diagnostics) | `branch_resolution` | Recovery | Section |
|---|---|---|---|
| On a branch that is not tracked | `fallback_ancestor` / `fallback_default` | `tracedecay branch add` | §4a |
| Tracked branch shows `[missing-db]` / `db_exists:false` | (any, with the missing-db warning) | remove + re-add (re-seed) | §4b |
| `branch_drifted:true` / `Opened branch ≠ Current` | `stale_serving_branch` | restart server (or reopen); add if untracked | §6 |
| Added a branch you're on, but server still serves ancestor | `fallback_*` with `live_branch_tracked:true` | restart MCP server | §6 |
| `tracking_enabled:false` unexpectedly (meta corrupt/missing) | `single_db` + orphaned `branches/*.db` | restore meta or re-add branches | §5 |
| Merged/deleted branches still tracked | (meta lists gone refs) | `tracedecay branch gc` | §4c |
| Wrong branch wrote into a DB (collision/aliasing) | two branches share one `db_file` | split DBs + re-index | §7 + INVARIANTS Risk A |

---

## §4. Common recovery actions **[mutating]**

### 4a. I'm on an untracked branch — track it

The live branch isn't in `branch-meta.json`, so reads fall back to an ancestor
and writes are refused. Track it (copies the nearest ancestor DB + incremental
sync):

```bash
tracedecay branch add                # [mutating] tracks the current branch
tracedecay branch add feature/parser # [mutating] track by name without checkout
```

> ⚠️ **CLI `branch add` collision caveat (INVARIANTS Risk A).** The CLI path
> (`commands.rs:192-202`) computes the DB stem with bare `sanitize_branch_name`,
> **not** the collision-safe `unique_branch_db_stem` the library/hooks use
> (`branch.rs:118`). If you add two branches whose sanitized names collide
> (e.g. `feature/foo` then `feature_foo`), the second `fs::copy` **overwrites**
> the first branch's DB and both meta entries alias the same file. Workarounds:
> - Prefer the agent/hooks auto-track path (Cursor `workspaceOpen`, Codex) which
>   uses the safe library path.
> - If you must use the CLI, check `tracedecay branch list` first and avoid names
>   that sanitize to an existing stem (slashes and most punctuation → `_`).

### 4b. A tracked branch lost its DB (`[missing-db]`)

The meta entry exists but `branches/<stem>.db` is gone (manual delete, crash,
disk full). Re-seed it by removing the orphaned meta entry and re-adding:

```bash
tracedecay branch remove hotfix/9    # [mutating] drops the orphaned meta entry + any sidecars
tracedecay branch add    hotfix/9    # [mutating] re-seeds from nearest ancestor + sync
```

`branch add` copies the nearest tracked ancestor's DB and runs an incremental
sync that only re-parses files whose content hash differs — usually cheap even
on large indexes. Verify with §7.

### 4c. Clean up branches that no longer exist in git

```bash
tracedecay branch gc                 # [mutating] deletes DBs for branches whose ref is gone
```

> ⚠️ **`branch gc` ref detection is filesystem-heuristic (INVARIANTS Risk C).**
> It checks `.git/refs/heads/<name>` and scans `.git/packed-refs`
> (`commands.rs:303-312`). On **linked worktrees** and **bare repos** refs
> resolve differently and a still-existing branch can be misclassified as stale
> and **deleted**. If you use those layouts, verify the "stale" list against
> `git for-each-ref refs/heads` first, or remove branches individually with
> `tracedecay branch remove <name>`.

### 4d. Other lifecycle commands

```bash
tracedecay branch remove <name>   # [mutating] stop tracking one branch, delete its DB + WAL/SHM
tracedecay branch removeall       # [mutating] remove every non-default branch
```

The **default branch cannot be removed.** `removeall` keeps only the default
branch DB. Both only touch branches listed in `branch-meta.json`.

---

## §5. Rebuild or copy a branch DB manually **[mutating]**

When the CLI re-seed (§4b) isn't usable (e.g. the ancestor DB itself is corrupt,
or you need to seed from a specific branch), you can perform the same DB-copy
operation the code does and include the SQLite sidecars for safety. The CLI's
core copy/meta-write path is in `commands.rs:192-203`:

1. **Stop the MCP server** so no instance holds the DB/WAL open (see §1).
2. Pick a healthy source DB — usually the default-branch DB or the nearest
   ancestor. Confirm it opens cleanly: `sqlite3 <src>.db "PRAGMA integrity_check;"`
   (expect `ok`).
3. Copy it to the target stem **with sidecars**:

   ```bash
   DATA_DIR="$(git rev-parse --show-toplevel)/.tracedecay"
   STEM="feature_parser"          # MUST match the entry in branch-meta.json → db_file
   SRC="$DATA_DIR/tracedecay.db"  # or branches/<ancestor>.db
   DST="$DATA_DIR/branches/$STEM.db"
   # copy all three; if a sidecar is absent, skip it
   cp -a "$SRC" "$DST"
   [ -f "$SRC-wal" ] && cp -a "$SRC-wal" "$DST-wal" || rm -f "$DST-wal"
   [ -f "$SRC-shm" ] && cp -a "$SRC-shm" "$DST-shm" || rm -f "$DST-shm"
   ```

4. The target `db_file` in `branch-meta.json` must be `branches/<STEM>.db`. If
   the entry is missing or points elsewhere, **don't hand-edit JSON blindly** —
   use `tracedecay branch remove <name>` then `tracedecay branch add <name>` and
   let the CLI fix the metadata.
5. Sync the copy so it reflects the branch's actual files:

   ```bash
   git checkout <target-branch>     # be on the branch you just seeded
   tracedecay sync                  # [mutating] hash-based delta against the new DB
   ```

> **Why `sync`?** A branch DB is seeded from an ancestor, so it starts holding
> the ancestor's graph. `tracedecay sync` compares on-disk content hashes against
> the DB and re-parses only the differing files (`index`/`sync` always target the
> serving branch's DB). Without it you'd serve the ancestor's symbols until the
> next watcher cycle.

### 5a. Corrupt `branch-meta.json`

If `branch-meta.json` is unparseable, the loader returns `None` and tracedecay
silently falls into **single-DB mode** (`branch_resolution: "single_db"`) — every
branch reads/writes the default DB, and any `branches/*.db` files become orphans
(INVARIANTS Risk D). Recovery:

1. Restore from the §1 backup if you have one.
2. Otherwise rebuild tracking from scratch:

   ```bash
   # [mutating] removes every non-default branch entry, then re-track what you need
   tracedecay branch add main   # if main isn't default, adjust; otherwise just:
   git checkout <branch> && tracedecay branch add   # repeat per branch you rely on
   ```

   Orphaned `branches/*.db` files left behind are harmless but waste disk; delete
   them by hand once you've confirmed none are referenced by the new meta.

---

## §6. Resetting serving-branch fallback

`serving_branch` is resolved once at `open()` time and cached on the running
instance (`tracedecay.rs:451-470`). The MCP server **hot-reopens on drift**
(`reopen_if_branch_drifted`, `mcp/server.rs:491`) — but only when the *live*
branch differs from the *open-time active* branch (`branch_drifted()`,
`tracedecay.rs:2947`). Two cases to know:

- **Live branch changed** (e.g. you switched from `main` to `feature/x`): the
  next MCP `tools/call` reopens and serves the right branch automatically. No
  action needed. (If `feature/x` is untracked it reopens into an ancestor
  fallback with a warning — fix with `tracedecay branch add`.)
- **Same live branch, but you just tracked it** (server opened before `branch
  add`, so it's serving an ancestor even though the live branch is now tracked):
  `branch_drifted()` is **false** (active == live), so there is **no auto-reopen**
  — the server keeps serving the stale ancestor. **Restart the MCP server** to
  force a fresh `open()` and re-resolve to the newly tracked branch's DB.

```bash
# [mutating-ish] restart the agent / MCP server so it re-resolves serving_branch.
# How depends on the host; e.g. for Hermes:
#   restart the agent, or re-run  tracedecay mcp   (the watcher picks up meta changes
#   on its next cycle, but a tracked-while-live branch needs the full restart above).
```

You do **not** need to restart after `branch add` if you then switch away and
back — the drift detector handles that. The restart is only for the
"tracked-while-already-on-it" case.

---

## §7. Verify recovery **[read-only]**

After any `[mutating]` action, re-run the diagnosis and confirm a clean state:

```bash
tracedecay branch list
```

A healthy result for a tracked branch you're on looks like:

```
Default branch: main
Current branch: feature/parser
Serving branch: feature/parser

  feature/parser [current, serving] — 207.1 MB (from main), synced just now
```

Checklist:

- `Current branch` == `Serving branch` (no `(fallback)`), and `[current,
  serving]` flags both set.
- No `warning:` lines.
- No `[missing-db]` flags; every tracked branch shows a size.
- In MCP `tracedecay_status`: `branch_resolution == "exact"`, `branch_drifted ==
  false`, `is_fallback == false`, `serving_db_exists == true`, empty
  `branch_warnings`.

If a write was previously refused, confirm it now succeeds: make a trivial
source change and run `tracedecay sync` — it should report the change against the
correct branch DB (the write gates in `ensure_branch_writable` re-check drift
and fallback, so a refused write is the signal that recovery isn't complete).

---

## §8. Reference: diagnostics fields & resolution values

### `branch_resolution` vocabulary (`tracedecay.rs:3124-3142`)

| Value | Meaning | Healthy? |
|---|---|---|
| `single_db` | No branch tracking (no/empty `branch-meta.json`). Drift/fallback guards exempt. | ✔ (if you never opted in) |
| `exact` | Live branch is tracked and its DB exists; serving it directly. | ✔ |
| `detached_default` | Detached HEAD; serving default DB with a warning. | ⚠ expected on detached HEAD |
| `fallback_ancestor` | Untracked live branch; serving nearest tracked ancestor. Writes refused. | ✘ → `branch add` |
| `fallback_default` | Untracked live branch, no usable ancestor DB; serving default. Writes refused. | ✘ → `branch add` |
| `stale_serving_branch` | Live branch differs from open-time active; instance is pinned/stale. | ✘ → reopen / restart (§6) |

### Key `BranchDiagnostics` fields (`tracedecay.rs:248-271`)

| Field | What it tells you |
|---|---|
| `tracking_enabled` | `false` ⇒ single-DB mode (no meta, or meta has no branches). |
| `current_branch` | Live git HEAD now. `None` = detached HEAD / not a repo. |
| `open_active_branch` | Branch the running instance pinned at `open()`. Differs from `current_branch` ⇒ drift. |
| `serving_branch` | Whose DB is actually open. `None` ⇒ single-DB mode. |
| `serving_db_path` / `serving_db_exists` | The DB file answering queries, and whether it's on disk. |
| `branch_drifted` | `current_branch != open_active_branch` while tracking is on. |
| `is_fallback` / `fallback_target` / `fallback_warning` | Serving an ancestor/default instead of the live branch. |
| `live_branch_tracked` / `live_branch_db_path` / `live_branch_db_exists` | Is the *current* git branch tracked, and does its DB exist? |
| `nearest_tracked_ancestor` (+ path/exists) | Which ancestor DB you'd seed from if you `branch add` now. |
| `branches[]` | Per-branch: `db_exists`, `size_bytes`, `parent`, `is_default/current/serving`, per-branch `warnings`. |

### Warning strings to expect (`tracedecay.rs:3079-3122`)

- *"branch drift detected: working tree is on 'X' but this instance opened on
  'Y' and is still serving 'Z'…"* → reopen/restart (§6).
- *"serving branch 'X' points at a missing DB: <path>"* → re-seed (§4b/§5).
- *"tracked branch 'X' is listed in branch metadata but its DB is missing…"* →
  re-seed (§4b).
- *"branch 'X' is not tracked; nearest indexed ancestor is 'Y'…"* → `branch add`
  (§4a).

---

## §9. What NOT to do

- **Don't** copy or delete a `.db` while the MCP server has it open with an
  active `.db-wal`. Stop the server first (§1) or use the CLI commands.
- **Don't** hand-edit `branch-meta.json` to "fix" a path you're unsure about — a
  bogus `db_file` can alias another branch's DB or trip the path-traversal guard
  (INVARIANTS Risk E). Use `branch remove` + `branch add`.
- **Don't** rely on `tracedecay status --json` for branch state — it emits
  `GraphStats` only. Use `tracedecay branch list` or the MCP tools/resources
  (§2).
- **Don't** run `branch gc` on linked worktrees / bare repos without
  cross-checking `git for-each-ref refs/heads` — the ref heuristic can delete a
  live branch's DB (INVARIANTS Risk C).
- **Don't** add two CLI branches whose sanitized names collide (`feature/foo`
  vs `feature_foo`) via `tracedecay branch add` — the CLI stem is collision-unsafe
  and will overwrite/alias (INVARIANTS Risk A).
- **Don't** delete `branches/*.db` files by hand to "clean up" — that creates
  `[missing-db]` entries. Use `branch remove` / `branch gc` so metadata stays
  consistent.

---

*Recovery runbook for the indexed code-graph multi-branch subsystem. Re-verify
`file:line` references after merges; the invariants and resolution vocabulary
are stable.*
