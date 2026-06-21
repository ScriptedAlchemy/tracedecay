# TraceDecay rebrand compatibility policy

Date: 2026-06-14
Source audits: `docs/TRACEDECAY-COMPATIBILITY-AUDIT.md`, `docs/TREESITTERS-RENAME-CONSTRAINTS.md`

This policy defines how the project treats names and artifacts from the pre-rebrand
TraceDecay era. It is normative for new code, docs, tests, release automation, and
agent/plugin integrations.

## Policy principles

1. **TraceDecay is the canonical name for new behavior.** New files, commands,
   config keys, docs, release assets, plugin IDs, and examples must use
   `tracedecay`, `.tracedecay`, `TRACEDECAY_*`, or `TraceDecay` unless this policy
   explicitly assigns the surface to a legacy-retention category.
2. **Compatibility must protect existing user data and installed agents.** Legacy
   project/user databases, branch metadata, monitor files, plugin installs, and
   shell/profile configuration must not be orphaned by a cosmetic rename.
3. **Compatibility shims must be intentional and tested.** Any code that accepts a
   legacy spelling must say whether it is retained indefinitely, auto-migrated,
   warning-producing, rejected, or externally constrained. Tests should cover the
   chosen category for runtime behavior.
4. **No silent destructive migration.** A fallback from `.tracedecay/` to
   `.tracedecay/` is not a license to rename, delete, or rewrite user data. Any
   future conversion command must be explicit, backed up, atomic, and reversible.
5. **Historical documents are not product surface.** Changelog entries, old plans,
   benchmark outputs, and TraceDecay-era narrative may keep old names when the name
   is part of the historical record.

## Category A — retained indefinitely without migration

These surfaces remain valid indefinitely. New code must continue to understand them
because removing them would hide existing data, break old installs, or erase useful
history.

### A1. Existing `.tracedecay/` project data directories

If a project root has `.tracedecay/` and no `.tracedecay/`, TraceDecay must continue
using `.tracedecay/` as the active data directory **in place**. It must keep reading
and writing `tracedecay.db`, branch metadata, curation sidecars, diagnostics target
artifacts, session sidecars, and other data rooted under that directory. It must not
auto-rename the directory or create a parallel `.tracedecay/` index that makes the
old index look lost.

If both `.tracedecay/` and `.tracedecay/` exist in the same project, `.tracedecay/`
is canonical and wins. The legacy directory remains user-owned data and must not be
deleted unless an explicit destructive command asks for it.

### A2. Existing `~/.tracedecay/` user data directories

If the user has `~/.tracedecay/` and no `~/.tracedecay/`, TraceDecay must keep using
`~/.tracedecay/` for user-scoped state such as `global.db`, monitor mmap/lock files,
and compatible caches. New users should default to `~/.tracedecay/`. If both exist,
`~/.tracedecay/` wins.

### A3. Legacy project discovery for maintenance commands

Commands that discover local project roots for listing, status, global accounting,
wipe/cleanup previews, or similar maintenance must recognize the canonical
`.tracedecay/tracedecay.db` layout and any explicitly enumerated pre-rename stores
during migration. Destructive commands must show the resolved legacy path clearly
before deleting anything.

### A4. Historical docs, changelog entries, benchmark outputs, and old daemon cleanup docs

Historical wording in `CHANGELOG.md`, `docs/TRACEDECAY-WHATSNEW.md`, old plans/specs,
benchmark reports, and daemon-removal instructions should remain unless it is
factually wrong. These references should be labeled as historical when helpful, not
mechanically rewritten.

### A5. Worldwide counter endpoint

The existing counter endpoint name may remain `tracedecay-counter` while the service
continues to be best-effort and documented. A future endpoint rename is allowed only
when the replacement worker is deployed and continuity/failure behavior is preserved.

## Category B — automatically migrated or cleaned up

These surfaces are accepted as old installed state, but installers, refreshers, or
uninstallers should rewrite or remove generated legacy artifacts so the post-rebrand
state has a single canonical TraceDecay entry.

### B1. Agent MCP server keys and generated prompt/rule markers

Agent integrations must install new entries under `tracedecay`. During install,
refresh, or uninstall they should detect generated `tracedecay` server entries, hook
commands, permissions, prompt headers, rule files, and managed plugin metadata and
reconcile them to the canonical `tracedecay` form or remove them when uninstalling.

Ownership checks are required. Generated legacy artifacts may be removed; unrelated
user-authored files under old directories must be preserved.

Examples: Cursor `rules/tracedecay.mdc`, old `## Prefer tracedecay MCP tools` prompt
blocks, Codex marketplace entries named `tracedecay`, Antigravity
`plugins/tracedecay.json`, Claude hook/config legacy shapes, and generated entries in
Zed/Cline/Kimi/OpenCode/Gemini/Kiro/Roo/Kilo/Copilot agent configs.

### B2. Hermes plugin/profile configuration aliases

Hermes config migration must treat these legacy values as old generated state and
move them to canonical TraceDecay configuration:

- `plugins.tracedecay.project_root` -> `plugins.tracedecay.project_root` when no
  current key exists.
- `plugins: ["tracedecay"]` -> `plugins: ["tracedecay"]` when enabling/refreshing.
- `provider: tracedecay` and context-engine `engine: tracedecay` -> the corresponding
  `tracedecay` provider/engine.
- Legacy `plugins/tracedecay` generated plugin directories and
  `skills/tracedecay/SKILL.md` -> removed or replaced by generated `tracedecay`
  artifacts, preserving unknown user files.

### B3. Legacy release archive extraction for explicit versions

When a user explicitly requests an old version, upgrade/install code must probe
current `tracedecay-*` assets first and then pre-rename release assets. Archive
extraction must prefer `tracedecay` / `tracedecay.exe` and then accept pre-rename
binaries for those old archives.

This is compatibility for explicit old-version requests only; it is not permission
to present old TraceDecay-only releases as the latest version.

### B4. Documentation wording for active compatibility behavior

Primary user docs should use TraceDecay as the canonical name while retaining short,
actionable compatibility notes where behavior exists. When behavior changes, docs
must be updated in the same change. Historical docs remain Category A instead of
being migrated.

## Category C — accepted as fallback but warning-producing

These surfaces are still honored, but the new spelling is canonical. Runtime code or
installer/reporting code should warn once per process or once per operation when the
legacy spelling is the one being used. Warnings should be concise, actionable, and
non-fatal.

Recommended warning format:

```text
warning: legacy TraceDecay setting <OLD_NAME> is deprecated; use <NEW_NAME> instead. TraceDecay honored <OLD_NAME> for this run.
```

When both old and new settings are present, the new setting wins. The preferred
warning is:

```text
warning: both <NEW_NAME> and legacy <OLD_NAME> are set; using <NEW_NAME>.
```

Warnings must not print secret values. They should name keys, paths, or config
locations only.

### C1. Legacy environment variable fallbacks

The following `TRACEDECAY_*` variables remain accepted as fallbacks for their
`TRACEDECAY_*` equivalents, with `TRACEDECAY_*` winning when both are set:

- Generic `TRACEDECAY_<suffix>` values read through the brand env helper.
- `TRACEDECAY_GLOBAL_DB`.
- `TRACEDECAY_ENABLE_GLOBAL_DB` / `TRACEDECAY_DISABLE_GLOBAL_DB`.
- `TRACEDECAY_RESEARCH_BLOCK_REASON`.
- `TRACEDECAY_PLUGIN_SUBAGENTS`.
- `TRACEDECAY_PROJECT_ROOT`.
- `TRACEDECAY_DISABLE_SUBPROCESS`.
- `TRACEDECAY_OFFLINE`.
- `TRACEDECAY_MODEL_PRICES_PATH`.
- `TRACEDECAY_PLUGIN_PATH`, but only where an implementation actually honors it; docs
  must not promise this fallback beyond verified code paths.

Boolean parsing should be made consistent before changing semantics. Today,
`DISABLE_TRACEDECAY` is exact-string `true`, while many `brand_env()` consumers use
truthy parsing. Treat this as a compatibility constraint until tests document any
intentional divergence.

### C2. Legacy disable variable for server opt-out

`DISABLE_TRACEDECAY=true` remains accepted as the legacy spelling for
`DISABLE_TRACEDECAY=true` and must continue to make `tracedecay serve` exit cleanly
so hosts do not retry. New docs and examples should use `DISABLE_TRACEDECAY=true`.
If a warning is emitted, it must not prevent the clean zero-effect exit.

### C3. Legacy docs-only plugin path claims

Docs that mention `$TRACEDECAY_PLUGIN_PATH` or `.tracedecay/plugins/` fallback must be
kept only where backed by implementation. If implementation is absent or unknown,
the doc should be phrased as a compatibility target/follow-up rather than a runtime
guarantee.

### C4. Homebrew/Scoop legacy package names

Docs may warn that external taps/buckets can lag behind the rename and still expose a
legacy package name. This is a user-support warning, not a new canonical install path.

## Category D — reject, fail, or do not silently accept

These surfaces should not be accepted silently because doing so creates downgrade,
data-loss, or supply-chain risk.

### D1. Legacy-only releases in latest-version detection

Automatic latest-version detection must reject releases that only contain legacy
`tracedecay-*` assets, including old pre-reset release epochs. Users may still request
specific old versions explicitly via Category B behavior, but the automatic updater
must not advertise legacy-only releases as current.

### D2. Silent storage renames or implicit data moves

Code must reject any implicit migration that renames pre-rename data directories or
databases, or moves user-level data roots without an explicit migration command. A
future migration command must require clear user intent and must handle backups,
branch metadata, session payloads, curation sidecars, monitor files, caches,
rollback, and both project and user data roots.

### D3. New public TraceDecay-branded product surface

New features must not introduce public pre-rename names for commands, configuration,
API fields, docs examples, package names, release assets, service identifiers, or
generated plugin artifacts unless the name is categorized here as legacy retention or
an external constraint.

### D4. Unowned cleanup

Installers and uninstallers must not delete unknown user-authored files merely
because they live under an old path. Generated files can be migrated or
removed; unrecognized files require preservation or explicit user confirmation.

## Category E — externally constrained names that must not change yet

These names are not compatibility shims owned by TraceDecay. They are external
upstream identifiers or ecosystem state that the project cannot safely rename by
itself.

### E1. `tracedecay-large-treesitters`

`tracedecay-large-treesitters` and its `tracedecay-medium-treesitters` /
`tracedecay-lite-treesitters` siblings must keep their exact legacy names while the
project depends on the external upstream package. The crate name, git repository,
crates.io package, build script, vendored grammars, and large -> medium -> lite
internal dependency chain are upstream-owned.

Do not rename, vendor, fork, or publish tracedecay-branded replacements for these
crates unless one of these policy-changing events occurs:

1. The upstream renames all three crates, the git repository, crates.io packages, and
   internal manifests consistently and publishes new versions; or
2. The project explicitly approves maintaining a full fork of the three-tier grammar
   pipeline, including build scripts, vendored grammars, registry publishing, and
   ongoing language updates.

Until then, keep `Cargo.toml` pointed at the upstream dependency, keep Rust imports
such as `tracedecay_large_treesitters::*`, and prefer the Rust-parser migration as the
long-term path to removing the dependency instead of renaming it.

## Compatibility surface map

Every audited surface maps to exactly one policy category below.

| Surface | Category | Required behavior |
|---|---:|---|
| Project `.tracedecay/` data dir and `tracedecay.db` | A | Use in place when `.tracedecay/` is absent; no auto-migration. |
| User `~/.tracedecay/` data dir, `global.db`, monitor files, cache defaults | A | Use in place when `~/.tracedecay/` is absent; new users default to `~/.tracedecay/`. |
| Project root discovery for `.tracedecay/tracedecay.db` | A | Continue detecting for list/status/wipe-style maintenance. |
| Branch metadata DB filenames in legacy dirs | A | Keep `tracedecay.db` for legacy active data dirs. |
| Dashboard curation preview under legacy active dir | A | Store under the active data dir, including `.tracedecay/` when that is active. |
| MCP schema text describing legacy project fallback | B | Keep docs/resources aligned with actual fallback behavior. |
| Diagnostics target dir under active data dir | A | Use active data dir; legacy projects use `.tracedecay/target`. |
| Generic `TRACEDECAY_*` / `TRACEDECAY_*` env fallback | C | Accept old as fallback, warn, and let new spelling win. |
| `DISABLE_TRACEDECAY=true` | C | Accept as clean serve opt-out; prefer `DISABLE_TRACEDECAY=true` in docs. |
| `TRACEDECAY_GLOBAL_DB` | C | Accept fallback with warning; `TRACEDECAY_GLOBAL_DB` wins. |
| `TRACEDECAY_ENABLE_GLOBAL_DB` / `TRACEDECAY_DISABLE_GLOBAL_DB` | C | Accept fallback with warning; keep test hermeticity for both names. |
| Hook/extraction env fallbacks (`TRACEDECAY_RESEARCH_BLOCK_REASON`, `TRACEDECAY_PLUGIN_SUBAGENTS`, `TRACEDECAY_PROJECT_ROOT`, `TRACEDECAY_DISABLE_SUBPROCESS`) | C | Accept fallback with warning; new names win. |
| Savings pricing env fallbacks (`TRACEDECAY_OFFLINE`, `TRACEDECAY_MODEL_PRICES_PATH`) | C | Accept fallback with warning; do not leak values in logs. |
| Hermes `plugins.tracedecay.project_root` | B | Migrate to/read as `plugins.tracedecay.project_root` when no current pin exists. |
| Hermes plugin list, memory provider, and context-engine aliases named `tracedecay` | B | Rewrite/remove to canonical `tracedecay` behavior. |
| `$TRACEDECAY_PLUGIN_PATH` and `.tracedecay/plugins/` docs claim | C | Warn/accept only where implemented; otherwise document as pending compatibility. |
| Generic agent config server key `tracedecay` | B | Reconcile generated old entries to one canonical `tracedecay` entry. |
| Legacy prompt/rule markers | B | Remove generated old blocks/files during install/uninstall/refresh. |
| Cursor legacy plugin directory and skill slugs | B | Remove generated old plugin artifacts; preserve unknown user files. |
| Hermes generated `plugins/tracedecay` directories and `skills/tracedecay/SKILL.md` | B | Migrate/remove generated artifacts; preserve unknown user files. |
| Codex legacy plugin directory and marketplace entry | B | Reconcile to canonical `tracedecay`; remove generated stale hooks/prompts. |
| Antigravity legacy CLI plugin path | B | Remove generated `tracedecay.json` on uninstall/reconcile. |
| Claude legacy hook/config migration | B | Repair/remove known generated legacy hook and config shapes. |
| `tracedecay-large-treesitters` package/dependency name | E | Keep exact upstream name until upstream rename or explicit maintained-fork policy. |
| Worldwide counter endpoint `tracedecay-counter` | A | Retain until replacement endpoint is deployed with preserved behavior. |
| Legacy release asset names for explicit requested versions | B | Probe after current asset names for explicit old-version installs/upgrades. |
| Latest-version filtering of legacy-only releases | D | Reject legacy-only/pre-reset releases as automatic latest candidates. |
| Extracted binary name `tracedecay` in old archives | B | Accept only as fallback while extracting explicit old archives. |
| Homebrew/Scoop legacy package note | C | Keep as support warning while external packages lag; not canonical install path. |
| Primary user docs mentioning fallback behavior | B | Keep aligned with runtime behavior; canonical examples use TraceDecay names. |
| Design docs mentioning path/env/plugin fallback | B | Keep aligned with runtime behavior or mark as compatibility target. |
| `docs/TRACEDECAY-WHATSNEW.md` historical narrative | A | Preserve as history. |
| Historical plans/specs with TraceDecay names | A | Preserve as history unless factually wrong. |
| Benchmark reports with TraceDecay tool/path names | A | Preserve measured historical names. |
| Changelog historical TraceDecay references | A | Preserve history; summarize rename in current entries only. |
| Legacy config field `include` unrelated to brand rename | B | Keep ordinary config compatibility/migration separate from brand policy. |
| LCM/session schema legacy data carry-forward unrelated to brand names | B | Treat as ordinary schema migration, not TraceDecay-brand compatibility. |

## Warning and migration implementation guidance

- Prefer one warning per legacy key/path per process or command invocation.
- Warnings must explain the replacement and state that the old spelling was honored
  for this run.
- Warnings must not include environment variable values, DB paths containing secrets,
  tokens, or raw config contents.
- Tests for fallback code should cover: old-only, new-only, both-set precedence, and
  warning/no-warning behavior where warning hooks exist.
- Installer migrations must be idempotent. Running install/refresh twice should not
  duplicate entries or re-delete preserved user files.
- Any future explicit storage migration command must run a dry-run by default, list
  every source/destination path, create backups, validate the migrated DBs/sidecars,
  and leave the legacy tree untouched unless the user explicitly requests cleanup.

## Review checklist for future rebrand-related changes

Before merging a change that touches `tracedecay`, `.tracedecay`, `TRACEDECAY_*`, or
TraceDecay-era docs, answer all of the following in the PR description or review:

1. Which policy category owns the surface?
2. Does the change preserve existing user data and installed agent state?
3. If it accepts a legacy spelling, does the new spelling win when both are present?
4. If it migrates/removes anything, is ownership proven and are unknown user files
   preserved?
5. If it rejects something, is the failure mode clear and covered by tests?
6. If it touches `tracedecay-large-treesitters`, has the upstream/fork policy changed?
