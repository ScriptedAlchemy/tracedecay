# Dashboard Port — Phase 2 Handoff

Status: integration gate passed (2026-06-10) — see "Final integration gate"
at the end of this doc. Phase 3 complete (curation implemented; hard-delete
semantics, no archive). Nothing committed; everything is in the working trees
of `/home/zack/projects/tokensave` and `/home/zack/hermes-agent`.

Hermes integration (2026-06-10): boot failure fixed, the tracedecay-backed
dashboard is render-verified inside a live `hermes dashboard`, and the old
holographic_plus dashboard tab is retired. See "Hermes live render +
holographic_plus retirement" below.

This doc records what was copied from where, the architecture, the endpoint
mapping, what works, what's stubbed, Phase 2 design changes, and concrete
Phase 3 TODOs.

## What was copied from where

| Destination (tracedecay repo) | Source | Notes |
|---|---|---|
| `dashboard/holographic/src/` (14 files) | `/home/zack/hermes-agent/plugins/memory/holographic_plus/dashboard/src/` | Full buildable TS/React source, unmodified |
| `dashboard/holographic/dist/style.css` | generated from `dashboard/holographic/src/styles.css` | Phase 2 replaced the frozen Tailwind artifact with a hand-rolled token stylesheet rebuilt by `dashboard/build.mjs` |
| `dashboard/holographic/build.from-hermes.mjs` | same plugin's `build.mjs` | Reference only; depends on `hermes-agent/web/node_modules` + host theme |
| `dashboard/holographic/manifest.json` | same plugin | Reference copy of the Hermes manifest |
| `dashboard/lcm/src/{index.js,style.css}` | `/home/zack/projects/lcm/dashboard/dist/` | The LCM repo ships no separate frontend source; its `dist/` is hand-written, unbundled, readable JS (React via SDK, no JSX) — treated as source |
| `dashboard/lcm/manifest.json` | `/home/zack/projects/lcm/dashboard/manifest.json` | Reference copy |
| `dashboard/shell/` | new | Standalone host shell (see architecture) |
| `dashboard/hermes-wrapper/` | new | Canonical source of the Hermes-side wrapper |

Deployed to Hermes (working tree of `/home/zack/hermes-agent`, replacing the
old hand-written hermes_intelligence dashboard per the "replace, don't shim"
direction):

| File | Content |
|---|---|
| `plugins/hermes_intelligence/dashboard/manifest.json` | copy of `dashboard/hermes-wrapper/manifest.json` |
| `plugins/hermes_intelligence/dashboard/plugin_api.py` | copy of `dashboard/hermes-wrapper/plugin_api.py` (reverse proxy) |
| `plugins/hermes_intelligence/dashboard/dist/{index.js,style.css,holographic.js,lcm.js}` | copy of `dashboard/hermes-wrapper/dist/` |

Untouched: `plugins/memory/holographic_plus/` (its own dashboard still runs
the old Python implementation — see TODOs) and every other Hermes plugin.

## Architecture

Layering requirement: the tracedecay dashboard (Rust server + UI bundles) is
the **canonical** implementation; the Hermes plugin is a **thin wrapper**
that reuses it, never a fork.

```
                ┌────────────────────────────────────────────┐
                │ UI bundles (byte-identical in both hosts)  │
                │  holographic/dist/index.js  (esbuild IIFE) │
                │  lcm/dist/index.js          (plain IIFE)   │
                └──────────────┬─────────────────────────────┘
            register via window.__HERMES_PLUGINS__ / SDK
           ┌───────────────────┴────────────────────────┐
           │ standalone                                 │ hermes-hosted
┌──────────▼─────────────┐                 ┌────────────▼─────────────────┐
│ shell/dist/shell.js    │                 │ hermes-wrapper dist/index.js │
│ (bundles React 19,     │                 │ (uses host SDK; evaluates    │
│  Hermes-compatible SDK)│                 │  child bundles via window    │
└──────────┬─────────────┘                 │  Proxy w/ rewritten fetchJSON│
           │ same-origin fetch             └────────────┬─────────────────┘
┌──────────▼─────────────────────┐         ┌────────────▼─────────────────┐
│ `tracedecay dashboard` (axum)   │◄────────│ plugin_api.py reverse proxy  │
│ src/dashboard/{mod,assets,     │  spawns │ /api/plugins/                │
│  memory_api,lcm_api,util}.rs   │ + HTTP  │   hermes-intelligence/*      │
└──────────┬─────────────────────┘         └──────────────────────────────┘
           │ SQL (libsql)
   ┌───────┴──────────────┬───────────────────────────┐
   │ project DB           │ global DB                 │
   │ .tracedecay/          │ ~/.tracedecay/global.db    │
   │   tracedecay.db       │  lcm_raw_messages,        │
   │  memory_facts,       │  lcm_summary_nodes,       │
   │  memory_entities,    │  lcm_summary_sources (+   │
   │  memory_banks, ...   │  FTS mirrors)             │
   └──────────────────────┴───────────────────────────┘
```

Key decisions:

- **UI bundles are unmodified.** Both UIs were written against the Hermes
  plugin SDK (`window.__HERMES_PLUGIN_SDK__` / `__HERMES_PLUGINS__`); instead
  of editing them, both hosts provide that SDK. The standalone shell
  (`dashboard/shell/`) implements the subset the bundles use (React + hooks,
  `fetchJSON`, Card/Badge/Button/Input/etc. shims, `cn`/`timeAgo`/
  `isoTimeAgo`, `useI18n`). The hermes wrapper passes through the real host
  SDK with only `fetchJSON`/`authedFetch` URL-rewritten.
- **API paths and payload shapes mirror the original plugin APIs** (the
  holographic `plugin_api.py` and hermes-lcm `plugin_api.py`), so the same
  bundles work against both the standalone server and (rewritten) the Hermes
  proxy.
- **Hermes wrapper avoids global patching.** Child bundles are fetched as
  text and evaluated with `new Function("window", ...)` against a Proxy, so
  concurrent loading of *other* Hermes plugins can never observe patched
  globals. Captured components are re-registered under one combined
  `hermes-intelligence` tab (internal Memory/LCM tabs).
- **Extension point for richer Hermes features:** `GET /api/capabilities`
  (standalone) / `GET /api/plugins/hermes-intelligence/capabilities`
  (Hermes, which overrides `mode: "hermes"`). Wrapper-side extras should add
  endpoints in `plugin_api.py` and merge flags into this payload; the UI can
  feature-detect rather than fork.

### Standalone command

```
tracedecay dashboard [--path <project>] [--host 127.0.0.1] [--port 7341]
```

- Registered in `src/cli.rs` (`Commands::Dashboard`), dispatched in
  `src/main.rs`, implemented in `src/dashboard/` (axum 0.8, new dep).
- `--port 0` binds a free port. First stdout line is stable and parseable:
  `tracedecay dashboard listening on http://127.0.0.1:PORT/` (the Hermes
  wrapper parses it).
- Static assets are **embedded at compile time** (`src/dashboard/assets.rs`
  uses `include_bytes!` of `dashboard/*/dist/*`), so run
  `cd dashboard && npm install && npm run build` before `cargo build` when
  the UI changes. `build.rs` emits rerun-if-changed for every dist file (so
  rebuilt assets re-embed automatically) and, when the dist files are missing
  entirely (fresh checkout / `cargo install --path .`), builds them itself via
  `npm ci`/`npm install` + `npm run build`, failing fast with instructions
  only when npm is unavailable. Crate packages ship the prebuilt dist files
  (`package.include` whitelist), so `cargo package`/`publish`/crates.io builds
  need no Node toolchain.

## Endpoint mapping (old API → tracedecay-backed API)

### Holographic memory (`/api/plugins/holographic`, `src/dashboard/memory_api.rs`)

Backed by the **project DB** (`.tracedecay/tracedecay.db`; a legacy
`.tokensave/` directory is still honored as a fallback). Old backend was
Hermes `~/.hermes/memory_store.db` (`facts`/`entities`/`memory_banks`).

| Route | Old source | New source | Status |
|---|---|---|---|
| `GET /` (overview+facts+entities+graph) | `facts`, `entities`, `fact_entities`, `memory_banks` | `memory_facts`, `memory_entities`, `memory_fact_entities`, `memory_banks` | working |
| `GET /projection` | numpy PCA over `hrr_vector` blobs (float64) | Rust dual-PCA (Gram matrix power iteration) over bincode-encoded `Vec<f64>` phase vectors | working |
| `GET /similarity` | pure-python `mean(cos(p_i−p_j))` + lexical overlap + classification | same math in Rust (`SIMILARITY_FACT_CAP` 500, identical thresholds) | working |
| `GET /archive` / `POST /archive/{id}/restore` | `facts.state='archived'` / provider restore | **removed by design** — tracedecay curation hard-DELETEs losing facts; there is no archive state and no restore. The UI's Archive tab was removed accordingly. | n/a |
| `GET /curation/status` | hermes curator state files | Returns `enabled:true`, `mode: similarity_dedup`, last preview timestamp | **working** |
| `GET /curation/activity` | curator activity events | Always empty (no LLM/agent event stream) | working |
| `GET /curation/preview` | saved dry-run file | Last `dry_run=true` result, persisted to a `.tracedecay/dashboard/curation_preview.json` sidecar (survives restarts); stale when fact count changes | **working** |
| `POST /curate` | `agent.memory_curator.run_memory_curation` | Similarity-based dedup: proposes/applies `delete` actions for `likely_duplicate` pairs; `dry_run=true` returns plan, `dry_run=false` hard-deletes losers via `MemoryStore::remove_fact` | **working** |
| `POST /curate/apply` | (new, no Hermes equivalent) | Generic curation-ops apply API: `{"ops": [{"op":"delete",...} \| {"op":"merge",...}]}` with per-op results; the contract for external (LLM) planners | **working** |
| `providers` block in `GET /` | hermes provider discovery | static tracedecay stub | stubbed |

Mapping notes: bank names are the category itself in tracedecay (old store
used `cat:<category>`); `dim` ← `hrr_dim`; timestamps are unix epoch seconds
(shell `timeAgo` handles them); FTS search for facts falls back to LIKE
(tracedecay has no `facts_fts`).

### LCM (`/api/plugins/hermes-lcm`, `src/dashboard/lcm_api.rs`)

Backed by the **global DB** (`~/.tracedecay/global.db`; overridable via
`TRACEDECAY_GLOBAL_DB` — legacy `TOKENSAVE_*` variable names are still honored
as fallbacks). Old backend was `$HERMES_HOME/lcm.db`
(`messages`/`summary_nodes` + FTS).

| Route | Status | Mapping notes |
|---|---|---|
| `GET /overview` | working | `messages`→`lcm_raw_messages`, `source`←`provider`, compression from `lcm_summary_nodes` token counts |
| `GET /search` | working (FTS + LIKE fallback, role/source/session/since/until facets) | `messages_fts`→`lcm_raw_messages_fts(index_text)`, `nodes_fts`→`lcm_summary_nodes_fts`; `since`/`until` accept epoch only (UI never sends them) |
| `GET /session/{id}` | working | `token_estimate` ≈ chars/4 (not stored); `pinned`=0, `tool_name`=null (not tracked) |
| `GET /node/{id}` | working | node ids are **strings** (old API: ints); `source_ids` JSON → `lcm_summary_sources` rows (`raw_message` source_id = store_id, `summary_node` = node_id) |
| `GET /timeline` | working | `strftime(..., 'unixepoch')` buckets; node recency = `COALESCE(source_time_end, created_at)` |
| `GET /compression` | working | `token_count` ← `summary_token_count` |

`category`/`tags`/`entities` for nodes come from
`json_extract(metadata_json, ...)` with `'general'` fallback.

## What works (verified)

- `cd dashboard && npm install && npm run build` — builds shell (React 19 +
  esbuild), **rebuilds the holographic bundle from src** (proves the source
  port is buildable), copies LCM, assembles the hermes-wrapper dist.
- `cargo check`, `cargo clippy --bin tracedecay` (repo denies
  `unwrap`/`expect`), `cargo test --lib dashboard` — clean.
- `tracedecay dashboard --port 7341` against this repo's own index: verified
  via curl + headless browser — overview (133 facts/685 entities/6 banks),
  trust histogram, growth, graph (23 nodes/29 edges), projection (PCA, dim
  2048), similarity (real `likely_duplicate` pairs), curation stubs, LCM tab
  renders with correct empty-state (local global.db has no LCM rows yet);
  LCM search exercises the FTS path (`engine: "fts"`).
- Hermes wrapper `plugin_api.py` tested with FastAPI `TestClient` in the
  hermes venv, both modes: external server (`TRACEDECAY_DASHBOARD_URL`) and
  subprocess spawn (`TRACEDECAY_BIN` + `--port 0` + stdout URL parse +
  shutdown). All proxied routes returned correct payloads.

## Phase 2 UI/design changes

- Added a coherent dark "graphite observatory" design system for the
  standalone shell: shared palette, typography, page chrome, tab treatment,
  focus states, scrollbars, cards, badges, inputs, buttons, loading, and error
  surfaces.
- Upgraded the standalone Hermes-compatible SDK component shims so `Button`
  honors `ghost`, `outlined`, `secondary`, `destructive`, and `size` props used
  by the ported bundles.
- Replaced `dashboard/holographic/dist/style.css` as a frozen Hermes/Tailwind
  artifact. `dashboard/build.mjs` now copies the source-built
  `dashboard/holographic/src/styles.css`, a compact token-based utility subset
  plus dashboard-specific styling.
- Improved memory dashboard readability across overview stats, category bars,
  HRR coverage, trust/growth charts, fact/entity/bank lists, Semantic Map,
  Association Graph, Similarity, and Curation panels. The PCA and graph views
  keep their existing interactions while gaining stronger legends, contrast,
  hover/focus treatment, and responsive layout.
- Polished the LCM dashboard with the same token vocabulary, clearer search and
  facet controls, stronger card/list/chart styling, and a first-class empty
  state for the current global DB case where `lcm_raw_messages` and
  `lcm_summary_nodes` are empty.
- Updated the Hermes wrapper tab chrome to match the standalone shell while
  preserving the thin wrapper architecture and URL-rewriting proxy behavior.

## Phase 2 verification

- `cd dashboard && npm run build` — clean; rebuilt shell, holographic JS/CSS,
  LCM dist copy, and wrapper dist. Bundle sizes after the redesign:
  `shell.js` 192.8 KB, `shell.css` 10.5 KB, `holographic/index.js` 319.7 KB,
  `holographic/style.css` 14.2 KB, `lcm/index.js` 42.5 KB, `lcm/style.css`
  22.5 KB, wrapper stylesheet 37.9 KB.
- `cargo check`, `cargo clippy`, and `cargo test --lib dashboard` — clean.
  Dashboard lib tests: 3 passed, 335 filtered.
- `cargo run -- dashboard --port 0` against this repo: browser-verified
  standalone Memory and LCM tabs. Memory rendered real data (133 facts, 685
  entities, 6 banks), internal view switching (Inspector, Semantic Map, Graph,
  Similarity), search filtering, and Similarity pairs. LCM rendered the zero-row
  global DB state with the new empty-state panel. Screenshots were inspected at
  desktop and ~420px width.
- Hermes wrapper external-URL mode verified via FastAPI `TestClient` under
  `/home/zack/hermes-agent` with `uv run python` and
  `TRACEDECAY_DASHBOARD_URL` pointed at the local dashboard server:
  `/capabilities`, `/holographic/`, `/holographic/projection`,
  `/holographic/similarity`, `/lcm/overview`, and `/lcm/search` all returned
  200. Capability payload reported `mode: "hermes"`; holographic overview
  reported 133 facts; LCM overview reported 0 messages.

## Phase 3 — Curation (done; hard-delete semantics, no archive)

> Note: an earlier in-flight revision of this phase implemented soft-archive
> (a `state` column + archive/restore endpoints). That was replaced before
> ever being committed: curation now hard-DELETES losing facts.

### Schema state (migration v13)

**No archive columns exist.** `memory_facts` keeps its pre-Phase-3 shape.
`migrate_v13` is a cleanup marker: it drops the never-shipped archive columns
(`state`, `archived_at`, `archived_reason`, `merged_into`, `superseded_by`)
and the `idx_memory_facts_state` index from any local development database
that briefly ran the earlier uncommitted revision; on all other databases it
is a no-op. `LATEST_VERSION` stays 13 (monotonic).

### Deletion semantics and normal recall

Curation deletes go through the canonical store path
(`MemoryStore::remove_fact`): a `BEGIN IMMEDIATE` transaction deletes the
`memory_facts` row, FK `ON DELETE CASCADE` removes `memory_fact_entities`
links, the FTS delete trigger removes the `memory_facts_fts` row, and the
fact's memory banks are marked dirty for rebuild. Because rows are physically
gone, deleted facts immediately disappear from `tracedecay_fact_store` recall,
FTS/entity candidate queries, and every dashboard view — no recall-path
filtering is needed (the earlier `state`-column filters were reverted).

Merge ops additionally rewrite the winner's content via
`MemoryStore::update_fact` (re-encodes the HRR vector and entity links) before
deleting the losers. The planner guarantees no plan action ever references a
fact that the same plan deletes (a fact consumed as a loser can't be a later
pair's winner), and the apply API validates a merge's winner exists before
touching anything.

### Curation semantics vs holographic_plus

The Hermes implementation delegates to `agent.memory_curator.run_memory_curation`
(LLM-backed agent with `archive`, `merge`, `supersede`, `retag`, `entity_*`, etc.).
The tracedecay backend does not have an LLM integration, so built-in curation is
**similarity-based deduplication**:

1. Runs the same pairwise phase-cosine similarity analysis as `GET /similarity`.
   The O(n²·d) scoring runs on the blocking pool (`spawn_blocking`, never inline
   on the async runtime) and is cached keyed by a fingerprint of the vectored
   fact state (count, max `updated_at`, id sum), with all pairs ≥ 0.5 stored so
   UI threshold tweaks never recompute.
2. For each pair classified `likely_duplicate` (similarity ≥ 0.95 + lexical
   overlap threshold), proposes to DELETE the **lower-trust** fact (`delete`
   action with `duplicate_of` pointing to the surviving winner).
3. `dry_run=true` returns the plan without mutating; the plan is saved in-memory
   as the "last preview" for `GET /curation/preview`.
4. `dry_run=false` executes the plan: each proposed loser is hard-deleted via
   `MemoryStore::remove_fact`. Clears the saved preview afterwards.
5. The response shape is a valid `MemoryCurateResponse` (`ran`, `dry_run`,
   `actions`, `counts: {delete: n}`, `applied_counts`, `llm_calls: 0`,
   `coverage`, `provider: tracedecay`, `mode: similarity_dedup`).

### Generic curation-ops apply API (contract for external planners)

`POST /api/plugins/holographic/curate/apply` accepts `{"ops": [...]}` where
each op is one of:

- `{"op": "delete", "fact_id": <id>, "reason": <string?>}`
- `{"op": "merge", "winner_id": <id>, "loser_ids": [<id>...], "merged_content": <string?>}`

Response: `{"results": [per-op result], "counts": {"deleted", "merged",
"errors"}}`. Ids are validated per-op; partial failures are reported per-op
(status stays 200), never as a whole-request 500. A 400 is returned only for a
malformed body. This is the contract the Hermes wrapper's future LLM curation
planner builds against.

### Capabilities

`GET /api/capabilities` returns `"curation": true, "llm_curation": false`
(standalone). The Hermes wrapper flips `llm_curation` when it adds an
LLM-backed planner that proposes merge/retag-style ops and applies them via
`/curate/apply`. The UI's CurationPanel consumes the same ops shape either
way (its Archive tab was removed; `delete` ops render as high-risk actions
with a permanent-deletion warning).

### Hermes live render + holographic_plus retirement (2026-06-10)

(Hermes-side follow-up to the Phase 3 TODOs below; separate from the
tracedecay-side curation/archive Phase 3 work.) All work here is confined to
`/home/zack/hermes-agent` (plus this doc). Nothing committed.

### Boot fix (`ModuleNotFoundError: No module named 'hermes_cli.middleware'`)

- Root cause: a working-tree change to `hermes_cli/plugins.py` (a "plugin
  middleware" refactor) replaced the inline constant
  `OBSERVER_SCHEMA_VERSION = "hermes.observer.v1"` with
  `from hermes_cli.middleware import OBSERVER_SCHEMA_VERSION, VALID_MIDDLEWARE`,
  but `hermes_cli/middleware.py` was never created (no git history, no stash;
  the only tracked `middleware.py` lives under `dashboard_auth/`). Importing
  `hermes_cli.plugins` (done during plugin discovery at boot) therefore raised
  `ModuleNotFoundError`. Confirmed it's a working-tree regression, not a
  stale-install / egg-info issue (the editable install runs the working tree;
  the failing import is the working-tree line).
- Fix (minimal): added `hermes_cli/middleware.py` defining the two symbols
  `plugins.py` imports — `OBSERVER_SCHEMA_VERSION = "hermes.observer.v1"`
  (verbatim original value) and `VALID_MIDDLEWARE = {"request", "execution"}`
  (the two kinds documented by `PluginContext.register_middleware`). Unknown
  middleware kinds only warn (never raise), so the set isn't load-bearing for
  boot. `import hermes_cli.plugins` now succeeds; `tests/hermes_cli/test_plugins.py`
  passes (73), as does `test_plugin_config_defaults.py` (9).

### How the dashboard was launched

Commands below are reproduced verbatim as run on 2026-06-10, pre-rebrand
(old `tokensave` binary and `TOKENSAVE_*` variable names; both are still
honored as legacy fallbacks):

```bash
# Build the working tokensave binary used by the wrapper (see note below).
# (target/debug/tokensave already had the dashboard subcommand.)

cd /home/zack/hermes-agent
TOKENSAVE_BIN=/home/zack/projects/tokensave/target/debug/tokensave \
TOKENSAVE_DASHBOARD_PROJECT=/home/zack/projects/tokensave \
HERMES_HOME=/home/zack/.hermes \
uv run --no-sync hermes dashboard --port 9215 --host 127.0.0.1 --no-open --skip-build
```

- `--skip-build` reuses the existing `hermes_cli/web_dist/` (avoids a Vite
  rebuild); `--no-open` keeps it headless.
- Auth: loopback bind ⇒ `app.state.auth_required is False`, so the OAuth gate
  (`dashboard_auth.gated_auth_middleware`) is a no-op. The legacy
  `_SESSION_TOKEN` is injected into the served `index.html` as
  `window.__HERMES_SESSION_TOKEN__`; the SPA sends it as
  `X-Hermes-Session-Token` automatically. No manual auth needed for local dev.

### Live render evidence (IDE browser, `http://127.0.0.1:9215/hermes-intelligence`)

- The combined **"Hermes Intelligence"** tab registers and renders (the
  `new Function` + window-`Proxy` bundle evaluation works under the real host;
  no CSP interference).
- **Memory** internal tab shows real tracedecay data: **133 facts / 685
  entities / 6 banks**, category bars (general 83 / tool 19 / code_area 14 /
  project 10 / decision 7), HRR coverage rings (100%), trust distribution,
  facts/day chart, and fact/entity/bank lists.
- **LCM** internal tab renders the first-class empty state ("No LCM sessions
  indexed yet") for the local `~/.tokensave/global.db` (zero LCM rows at the
  time; pre-rebrand path), with the `Database detected` badge.
- Spawn confirmed: with no `TOKENSAVE_DASHBOARD_URL` set (pre-rebrand variable
  name), the wrapper spawned
  `tokensave dashboard --host 127.0.0.1 --port 0 --path
  /home/zack/projects/tokensave` as a **child of the hermes python process**
  (verified via PPID), and `/api/plugins/hermes-intelligence/capabilities`
  returned `mode: "hermes"`.

Note on the binary (then named `tokensave`): a concurrent effort in this repo
was mid-flight on a curation/archive **v13 schema migration**. After it rebuilt
`target/debug/tokensave` (09:03), fresh spawns failed while opening the v12
project DB (`v13: failed to add archive columns to memory_facts: SQLite
failure: near "EXISTS": syntax error`) — its WIP, not the wrapper's. The live
post-retirement screenshots were therefore taken with the wrapper pointed at a
healthy standalone server via `TOKENSAVE_DASHBOARD_URL=http://127.0.0.1:7350`
(the task-sanctioned external-mode fallback). Spawn mode itself was verified
working earlier with the pre-rebuild binary. Once the v13 migration lands
cleanly, spawn mode needs no wrapper changes.

### holographic_plus dashboard retirement

- Mechanism: renamed
  `plugins/memory/holographic_plus/dashboard/manifest.json` →
  `manifest.json.disabled`. Dashboard discovery
  (`web_server._discover_dashboard_plugins`) keys solely off the presence of
  `dashboard/manifest.json`, so this single rename retires the tab **and**
  unmounts its `/api/plugins/holographic` routes. No enable/disable flag
  exists in discovery; the rename is the surgical, reversible disable. The
  manifest's `override: /holographic-memory` referenced no real built-in route
  (grep of `web/` + `hermes_cli/` found none), so nothing stale resurfaces.
- Memory functionality untouched: `register(ctx)` only calls
  `ctx.register_memory_provider(...)`; it never registered the dashboard.
- Verified after restart: dashboard plugin list went from
  `[..., holographic, hermes-intelligence, ...]` to
  `['agent-map', 'hermes-lcm', 'hermes-achievements', 'hermes-intelligence',
  'kanban']` (holographic gone, all others intact); the live nav no longer
  shows "Holographic Memory" while "Hermes Intelligence" remains;
  `GET /api/plugins/holographic/` now 404s.

### Verification (tests + boot)

- `tests/plugins/memory/test_holographic_plus_provider.py` +
  `_curator.py` + `_curator_tools.py` + `_ingress.py`: **126 passed** (memory
  provider intact post-retirement).
- `tests/hermes_cli/test_plugins.py`: **73 passed**;
  `tests/hermes_cli/test_plugin_config_defaults.py`: **9 passed**.
- hermes `dashboard` boots cleanly and serves; the retirement breaks no other
  plugin (LCM, Agent Map, Achievements, Kanban tabs all still register).
- Pre-existing, out-of-scope finding (NOT caused by this work, does NOT affect
  the dashboard or boot): the same `plugins.py` middleware refactor also
  removed `PluginContext.register_config_defaults`, which the committed
  `plugins/hermes_intelligence/__init__.py:141` still calls unguarded — so the
  `hermes_intelligence` **memory plugin** fails to load (logged, non-fatal;
  `config.py`'s reader is `try/except`-guarded, so config + boot are fine).
  Recommended reconciliation by the middleware-refactor owner: restore
  `register_config_defaults` (the removal left callers + the config path
  intact, suggesting it was unintended) or update/guard the lone caller. The
  pre-port `tests/plugins/test_hermes_intelligence_dashboard.py` suite (31
  failures) asserts the old hand-written dashboard bundle/API and is stale
  vs. the thin wrapper that replaced it — also pre-existing, unrelated to this
  task.

### Wrapper LLM curation + lifecycle hardening (2026-06-10, later)

Canonical wrapper (`dashboard/hermes-wrapper/plugin_api.py`) updated and
synced verbatim to the deployed copy
(`hermes-agent/plugins/hermes_intelligence/dashboard/plugin_api.py`), along
with freshly rebuilt dist bundles + manifest (includes the new graph.js).

**LLM curation (Hermes-only layer).** Ported from the holographic_plus
curator's one-shot LLM review tier (`_call_llm_oneshot` +
`_LLM_SYSTEM_PROMPT` + strict-JSON verdict parsing), adapted to the
`POST /curate/apply` contract (no archive; hard delete + merge only):

- `POST /api/plugins/hermes-intelligence/curation/llm-plan`
  (`{dry_run=true, limit, threshold, max_clusters, min_confidence}`):
  fetches `/similarity` pairs (likely_duplicate + merge_candidate) from the
  tracedecay server, union-find clusters them, sends ONE
  `agent.auxiliary_client.call_llm(task="memory_curator", temperature=0)`
  call (same task key as the original curator, so provider/model resolution
  matches), validates proposed ops (op vocabulary {merge, delete, keep},
  evidence guard: only reviewed fact ids, confidence floor), and either
  returns the plan (dry-run) or POSTs contract-shaped ops to
  `/api/plugins/holographic/curate/apply` and surfaces the per-op results.
  Original verdicts → contract mapping: merge→merge, supersede→delete,
  reflect→merge+merged_content; recategorize/retag have no contract op.
- `/capabilities` override now also sets `llm_curation: true` (top-level and
  `features.`) when the Hermes auxiliary client is importable — standalone
  tracedecay reports `false`, so UIs can feature-detect.
- Contract verified live against the rebuilt tracedecay binary (no
  mismatches): dry-run plan over real similarity clusters, then real apply
  against a **copy** of the project DB — `counts: {merged: 1}`, 63 losers
  hard-deleted, facts 133→70. A live in-hermes dry-run with the REAL
  configured LLM returned 200 and conservatively kept a 15-member related
  cluster (no ops) — the conservative prompt behaves as intended.

**Lifecycle fixes (adversarial-review findings).** All in `_spawn_dashboard`
/ `_shutdown` / `_upstream_base`:

1. stderr is now drained for the child's whole lifetime by a daemon thread
   (bounded tail kept for error detail), and the stdout reader keeps
   draining after the URL line — previously a full ~64KB pipe buffer would
   block the Rust server and 502 every proxied request.
2. Linux parent-death guard: `preexec_fn` sets `PR_SET_PDEATHSIG=SIGTERM`,
   so the spawned server dies with the Hermes host even on SIGKILL/OOM
   (atexit alone orphaned it). Dead-instance reap before each spawn.
3. Failure path no longer calls `communicate()` against the drain threads
   (single reader per pipe); `kill()` fallbacks now `wait()` to reap
   zombies.
4. Spawn failures are cached for a 30s backoff window: requests fail fast
   with a clear 503 (`retrying in Ns. Last error: ...`) instead of
   serializing every request behind repeated 30s spawn attempts under the
   module lock.

**Tests** (all passing, hermes venv):

- `tests/plugins/test_hermes_intelligence_llm_curation.py` (6): full
  op-generation → apply pipeline via FastAPI TestClient against an
  in-process contract-shaped tracedecay stub with seeded facts; LLM stubbed
  at the `call_llm` seam. Covers capabilities flag, dry-run validation
  (hallucinated ids + low-confidence rejected, keep filtered), apply posting
  contract-shaped ops only, no-clusters short-circuit, non-JSON LLM → 502,
  missing aux client → 503.
- `tests/plugins/test_hermes_intelligence_wrapper_lifecycle.py` (3): chatty
  fake binary (256KB stderr burst + endless two-stream chatter) keeps
  serving 20+ proxied requests with no hang; failed spawn surfaces stderr
  tail in the 503, fast-fails within the backoff window, leaves no zombie;
  host SIGKILL → spawned child exits via PDEATHSIG (no orphan).
- Combined sweep with `test_holographic_plus_provider.py` +
  `test_plugins.py`: **100 passed**.

Note: the wrapper's default clustering can chain many merge_candidate pairs
into one large cluster (union-find transitivity); the real LLM is the
conservatism backstop, and callers can pass a higher `threshold` /
`min_confidence`. Consider an upstream per-cluster size cap later.

## What's stubbed / known gaps

1. **Curation activity stream**: `GET /curation/activity` always returns an empty
   event list. The holographic_plus backend streams structured phases from a live
   LLM agent run; the similarity-dedup implementation has no equivalent events.
2. **Rich curation ops**: the built-in planner only proposes `delete`. The apply
   API additionally executes `merge` (content rewrite + loser deletion), but
   `supersede`, `retag`, and `entity_*` ops from holographic_plus are not
   implemented; an LLM planner (Hermes wrapper, `llm_curation` flag) is expected
   to supply richer plans via `/curate/apply` later.
3. **Preview persistence**: RESOLVED — the dry-run preview is mirrored to a
   sidecar (`.tracedecay/dashboard/curation_preview.json`) and re-hydrated on
   server start; applying curation clears both copies. API shape unchanged.
4. **Similarity floor**: RESOLVED — the cached pair set keeps every finite
   phase-cosine pair (`SIMILARITY_PAIR_FLOOR = -1.0`), and the API accepts a
   `min_similarity` parameter clamped to [-1, 1], so callers can brush below
   the UI's default duplicate-review floor without recomputation.

## Remaining stubbed / known gaps (post Phase 3)

1. LCM data scope: MOSTLY RESOLVED — the standalone dashboard now serves the
   project-local `.tracedecay/sessions.db` (where transcript ingest writes),
   with `TRACEDECAY_GLOBAL_DB` pinning an explicit store (also the path for
   hermes-profile stores: point it at `<hermes_home>/.tracedecay/sessions.db`).
   Remaining: an in-UI store *switcher* for browsing multiple stores.
2. The wrapper picks the project root from `TRACEDECAY_DASHBOARD_PROJECT` or
   Hermes' cwd — no per-request/workspace project selection.
3. The wrapper UI is now also rendered + verified inside a live
   `hermes dashboard` session (Hermes live-render section below), in addition
   to the earlier structural / FastAPI proxy validation.
4. The old holographic_plus dashboard tab is **retired** (Phase 3). The
   plugin's memory provider/tools/curator remain active; only its dashboard
   UI registration was disabled, since the `hermes-intelligence` wrapper now
   provides that UI backed by tracedecay.

## TODOs for Phase 3 testing/hardening

- [x] Render-test the wrapper inside a running `hermes dashboard`; confirm
      the `new Function` + window-Proxy evaluation works under the real host
      (no CSP found in `hermes_cli/web_server.py`, but verify), and that the
      combined tab looks right with host styling.
      - 2026-06-10 DONE. See "Hermes live render + holographic_plus
        retirement" below. The `new Function` + window-Proxy evaluation works
        under the live host; the combined "Hermes Intelligence" tab renders
        both Memory (real data: 133 facts / 685 entities / 6 banks) and LCM
        (empty-state) internal tabs. The earlier boot blocker
        (`ModuleNotFoundError: No module named 'hermes_cli.middleware'`) was
        root-caused and fixed (new `hermes_cli/middleware.py`).
- [x] Decide the fate of `plugins/memory/holographic_plus/dashboard` in
      hermes (retire or point it at the tracedecay backend too).
      - 2026-06-10 DONE — retired. The holographic_plus dashboard *registration*
        is disabled (its `dashboard/manifest.json` renamed to
        `manifest.json.disabled`, the sole discovery key). The "Holographic
        Memory" tab and its `/api/plugins/holographic` routes no longer mount;
        the holographic_plus **memory provider** (tools, curator, retrieval,
        `on_session_end`) is fully intact. See the Hermes live-render section
        below.
- [ ] LCM: surface provider/profile selection (project-local vs global vs
      hermes-profile stores); consider storing real `token_estimate`,
      `tool_name`, `pinned` in `lcm_raw_messages` so the session drawer
      stops approximating.
      - 2026-06-10 PARTIAL — project-local vs global selection shipped. The
        dashboard now serves the project's `.tracedecay/sessions.db` (where
        Cursor hooks + the hookless-agent catch-up sweep actually ingest)
        instead of the always-empty `~/.tracedecay/global.db`; a
        `TRACEDECAY_GLOBAL_DB` override still pins the store (smoke harness /
        Hermes wrapper contract, which is also the path for hermes-profile
        stores: point the override at `<hermes_home>/.tracedecay/sessions.db`).
        Additive `storage_scope` field on every LCM payload + `lcm_scope` in
        capabilities; LCM header shows "Project store"/"Global store".
        `tracedecay dashboard` startup now spawns the same detached catch-up
        ingest sweep as `tracedecay serve`. Remaining: an in-UI store
        *switcher* (multi-store browsing) and real `token_estimate` /
        `tool_name` / `pinned` columns.
- [x] Wire curation: tracedecay-side fact maintenance (dedupe via the
      existing similarity classification, trust decay) behind the
      `features.curation` capability flag; then enable the CurationPanel.
      - Implemented as similarity-based dedup with hard-delete semantics:
        `POST /curate` proposes/applies `delete` actions for `likely_duplicate`
        pairs, and `POST /curate/apply` exposes a generic delete/merge ops
        contract for external planners. No schema change shipped (v13 only
        cleans up a never-committed archive-column experiment). See the
        Phase 3 section above for full details.
- [x] Add `--open` (launch browser). Done: `tracedecay dashboard --open` opens
      the URL in the default browser after the server starts. Auth for
      non-loopback binds remains future work.
- [x] Tests: Rust integration tests for the two API modules against a seeded
      temp DB (mirroring `tests/mcp_handler_test.rs` patterns); a Playwright
      smoke for the standalone shell covering desktop/narrow viewports, search,
      tab switching, and visualization tooltips/hover states.
      - Added `tests/dashboard_api_test.rs` with 3 integration tests:
        holographic endpoint shape/value assertions, LCM seeded FTS + LIKE
        fallback coverage, and LCM empty-state assertions.
      - Added repeatable browser smoke: `dashboard/smoke.mjs` and
        `npm run smoke` (`--expect-lcm=empty|non-empty`) covering tab render,
        tab switching, holographic search interaction, Similarity view, and
        desktop + ~420px narrow viewports.
- [x] Ingestion: local `~/.tracedecay/global.db` has zero LCM rows — verify
      end-to-end with a session ingest before polishing the LCM charts.
      - Verified via a seeded temporary global DB mounted through
        `TRACEDECAY_GLOBAL_DB`; live `/api/plugins/hermes-lcm/overview` returned
        non-empty totals (`messages_total: 2`, `summary_nodes_total: 1`) and
        smoke coverage passed in `--expect-lcm=non-empty` mode.
- [x] Add a build-system guard so frontend dist changes always force
      re-embedding in local Rust builds, instead of requiring a manual touch or
      clean rebuild.
      - `build.rs` now emits `cargo::rerun-if-changed` for all embedded
        dashboard dist assets and exports `TRACEDECAY_DASHBOARD_ASSET_STAMP`.
      - `src/dashboard/assets.rs` threads that stamp into
        `x-tracedecay-asset-stamp` responses. After touching
        `dashboard/lcm/dist/style.css`, Cargo marked `tracedecay` dirty and
        reran the build script + recompilation path.

## How to run

```bash
# 1. Build UI assets (required before cargo build when UI changed)
cd /home/zack/projects/tokensave/dashboard && npm install && npm run build

# Optional smoke checks (browser automation)
# Empty-state LCM:
TRACEDECAY_GLOBAL_DB=/tmp/tracedecay-dashboard-lcm-empty.db npm run smoke -- --expect-lcm=empty
# Seeded/non-empty LCM:
TRACEDECAY_GLOBAL_DB=/tmp/tracedecay-dashboard-lcm-nonempty.db npm run smoke -- --expect-lcm=non-empty

# 2. Build + run standalone
cd /home/zack/projects/tokensave
cargo build --bin tracedecay
./target/debug/tracedecay dashboard            # http://127.0.0.1:7341/

# 3. Hermes-hosted (wrapper spawns the server automatically)
TRACEDECAY_BIN=/home/zack/projects/tokensave/target/debug/tracedecay \
TRACEDECAY_DASHBOARD_PROJECT=/home/zack/projects/tokensave \
hermes dashboard   # → "TraceDecay" tab (named "Hermes Intelligence" at port time)
```

## Final integration gate (2026-06-10)

A whole-feature verification pass across both working trees, after all
parallel agents finished. Everything below was run fresh, in order; nothing
committed.

- **Frontend**: `npm install` + `npm run build` clean (shell, holographic,
  lcm, graph, hermes-wrapper bundles); `node run-unit-tests.mjs` 16/16.
- **Rust**: `cargo check` clean; `cargo clippy --workspace --all-targets`
  clean after fixing 5 small lints, all in new dashboard/build code
  (`build.rs` manual `unwrap_or_default`, doc backticks + single-char
  bindings in `memory_api.rs`, `float_cmp` in `memory_analysis.rs` planner
  tie-break — now `total_cmp` — and its tests); full `cargo test` 2267
  passed / 0 failed.
- **Smoke**: `npm run smoke` passed in both `--expect-lcm=empty` and
  `--expect-lcm=non-empty` modes (seeded temp global DBs).
- **Live browser pass** against this repo: all three tabs in dark + light
  themes at desktop and ~420 px. Memory (inspector, semantic-map encodings,
  similarity histogram brush, curation dry-run preview — not applied), LCM
  (empty state + server-loss error state with retry affordances), Code Graph
  (overview analytics, search-to-focus, caller expansion 81→117 nodes, path
  mode: found path highlighted + no-path banner). `/api/capabilities`
  coherent: `curation: true`, `llm_curation: false`, `graph: true`, no
  archive flag.
- **Hermes sync**: canonical `plugin_api.py` + `manifest.json` already
  identical on both sides (LLM curation + lifecycle fixes included); stale
  hermes-side `dist/` bundles replaced with the freshly built canonical dist
  (hashes verified identical). Wrapper FastAPI TestClient checks passed in
  BOTH external-URL and spawn modes (capabilities, holographic overview +
  similarity, lcm overview, graph overview + search — 12/12), plus the
  existing `test_hermes_intelligence_llm_curation.py` +
  `test_hermes_intelligence_wrapper_lifecycle.py` suites (9 passed).
- **Docs**: README + `docs/dashboard.md` updated for the Code Graph tab, the
  `tracedecay_dashboard` MCP tool, `--open`, hard-delete curation, JSON error
  contracts (400/404/422 with `{"detail"}`), `storage_scope` badges, and the full
  environment variable matrix (TRACEDECAY_GLOBAL_DB, TRACEDECAY_BIN,
  TRACEDECAY_DASHBOARD_PROJECT, TRACEDECAY_DASHBOARD_URL, HERMES_HOME,
  TRACEDECAY_OFFLINE, DISABLE_TRACEDECAY). CHANGELOG `[Unreleased]` covers the
  dashboard feature set.

**Documentation coordination note:** The **Savings & Cost** dashboard tab is
under active development by a parallel agent. README.md and docs/dashboard.md
reference it as "(under active development)" to avoid duplicating work-in-progress
documentation that may change before the feature lands.
