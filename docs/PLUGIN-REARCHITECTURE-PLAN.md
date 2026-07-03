# TraceDecay Plugin / Skill Rearchitecture — Consolidated Implementation Plan

**Status:** Executable plan (planning doc only — no code changes ship from this branch).
**Branch:** `docs/plugin-rearchitecture-plan`.
**Prerequisite:** Starts **after** the in-flight `feat/claude-plugin-bundle` PR merges to `master`.
**Scope:** Collapse the three duplicated host bundles into one shared `plugin/` tree with per-host
manifest overlays; rework the Rust embed/deploy layer; unify the test/lint surface; refine the
skill catalog; and universalize the `using-tracedecay` bootstrap injection.

This document synthesizes three prior design passes (Packaging, Catalog+Adoption, Canonical Format)
into one ordered, dependency-aware migration. Claims below are verified against the code and cited
as `file:symbol` / `file:Lnn`.

---

## 0. Ground truth (verified against current `master`)

The following facts anchor the plan. Where a design pass assumed a target-state file, the current
reality is noted so the executing engineer is not surprised.

### 0.1 Bundles that exist today
- `cursor-plugin/` and `codex-plugin/` exist. **`claude-plugin/` does not exist on `master`** — it
  is introduced by the in-flight `feat/claude-plugin-bundle` branch (which, as of this writing, has
  **no diff vs `master`** yet: `git diff --stat master...feat/claude-plugin-bundle` is empty). Treat
  claude-plugin as a *future seed*, not an existing input.
- `cursor-plugin/` top-level files: `.cursor-plugin/plugin.json`, `README.md`, `mcp.json`,
  `hooks/hooks.json`, `rules/tracedecay.mdc`, `rules/tracedecay-memory.mdc`,
  `agents/{code-explorer,code-health-auditor,session-historian}.md`, and `skills/*/SKILL.md`.
- `codex-plugin/` top-level files: `.codex-plugin/plugin.json`, `.mcp.json`, `README.md`,
  `hooks/hooks.json`, and `skills/*/SKILL.md`. (No `agents/`, no `rules/`.)

### 0.2 Skill sets today
- **Shared/foundational skills** (present in both bundles, `disable-model-invocation` absent →
  model-invocable): `assessing-impact`, `code-health`, `curating-project-memory`, `editing-safely`,
  `exploring-code`, `fixing-build-and-type-errors`, `inspecting-managed-skills`,
  `recalling-project-memory`, `recalling-session-context`, `reviewing-changes`, `tracing-functions`,
  `using-the-cli`, `using-tracedecay`. These 13 are enumerated in
  `src/hooks/steering.rs:CURSOR_PLUGIN_SKILLS` (`src/hooks/steering.rs:15`).
- **Cursor-only dispatcher skills** — 13 `tracedecay-*` slash dispatchers carrying
  `disable-model-invocation: true` (verified: `grep -rl disable-model-invocation cursor-plugin/skills`
  returns 13). Examples: `tracedecay-review-diff`, `tracedecay-find-impact`, `tracedecay-check-health`,
  `tracedecay-compare-branches`, `tracedecay-port-code`, `tracedecay-test-changes`,
  `tracedecay-audit-safety`, `tracedecay-curate-memory`, `tracedecay-draft-commit`,
  `tracedecay-clean-dead-code`, `tracedecay-fix-build`, `tracedecay-recall-memory`,
  `tracedecay-map-architecture`. These are **not** in codex-plugin.

### 0.3 Rust embed / deploy mechanism (the load-bearing detail)
- Each host installer holds a **flat** `&[(&str, &str)]` table of `include_str!`s:
  - `src/agents/cursor.rs:199` — `const EMBEDDED_PLUGIN_FILES: &[(&str, &str)]`.
  - `src/agents/codex.rs:252` — `const CODEX_EMBEDDED_PLUGIN_FILES: &[(&str, &str)]`.
  - `feat/claude-plugin-bundle` will add the Claude analogue (`CLAUDE_EMBEDDED_PLUGIN_FILES` or
    equivalent) — do not assume its exact name; discover it post-merge.
- Deploy writes each `(relative, contents)` pair to the install dir
  (`src/agents/cursor.rs:390`, `src/agents/codex.rs:546`).
- **The embed side is SKILL.md-only / flat.** Every skill contributes exactly one `include_str!` for
  its `SKILL.md`; there is no `references/`, `scripts/`, or `assets/` entry anywhere in the tables.
- **Note the asymmetry that Design 3 slightly overstated:** the *deploy-from-cache* helper
  `collect_regular_files_inner` (`src/agents/cursor.rs:566`) already walks recursively. The gap is
  purely on the **compile-time embed** side — `include_str!` cannot glob, so support files simply are
  not embedded today. The fix (Phase 6) is to move embedding from a hand-maintained `include_str!`
  table to a build-time recursive manifest (via `build.rs` codegen or the `include_dir` crate).

### 0.4 Steering / bootstrap injection
- `src/hooks/steering.rs:build_cursor_session_context` (`:52`) injects the **full**
  `using-tracedecay` SKILL body via `append_tracedecay_bootstrap_context` (`:34`), sourced from
  `include_str!("../../cursor-plugin/skills/using-tracedecay/SKILL.md")` (`:32`). Codex reuses this.
- `src/hooks/claude.rs:claude_session_context_for_event` (`:126`) emits **only an
  `index_status_line`** — no bootstrap body. This is the adoption gap Design 2 flags.
- `src/agents/kiro.rs` uses `## Prefer tracedecay MCP tools` steering (`src/agents/kiro.rs:33`,
  `PROMPT_MARKER`) — softer "Prefer" language, not the `<EXTREMELY_IMPORTANT>` bootstrap.
- **In-flight (do not re-plan):** a separate agent is adding the Claude + Kiro bootstrap injection.
  This plan references that work as a dependency and hardens the *shared* bootstrap text once.

### 0.5 Test / lint surface today
- `tests/agent_suite/plugin_bundle_sync_test.rs` (424 lines) — cross-bundle byte-parity enforcement
  over `BUNDLES = [cursor-plugin, codex-plugin]`, with declarative exception tables. **This entire
  test becomes moot with one shared tree** (nothing to keep in sync).
- `src/agents/cursor.rs::embedded_file_list_covers_the_whole_source_bundle` (`:1123`) and
  `src/agents/codex.rs::codex_embedded_file_list_covers_the_whole_source_bundle` (`:1587`) — assert
  the flat embed table exactly covers the on-disk tree. Must be **rewritten** over the composed view.
- `src/agents/codex.rs::codex_skills_match_the_cursor_source_for_parity` (`:1638`) — the codex↔cursor
  skill parity check. **Delete** (moot with one copy).
- `tests/agent_suite/skill_lint_cursor_test.rs` (382 lines) — Cursor slash/`disable-model-invocation`
  contract, incl. the `/slug` H1 lint (`:152`) and the "stale slug strands the agent" reference lint
  (`:270`). **Retarget** the slash/slug parts at `.cursor/commands/`; the model-invocable frontmatter
  parts fold into the unified contract.
- `tests/agent_suite/plugin_skill_contract_test.rs` (448 lines) — per-host frontmatter schema +
  skill-creator design budgets. **Rewrite** as the single unified contract over one `skills/` tree
  plus host-extra assertions.

---

## 1. Target directory layout

One `plugin/` tree holds all shared content once. Per-host manifests live in dot-dirs and point at the
shared dirs via **plugin-root-relative** paths (validated first — see §7.1). Cursor's slash dispatchers
move out of `skills/` into Cursor-native `.cursor/commands/`.

```
plugin/
├── skills/                                  # ONE model-invocable set (13 foundational, see §6)
│   ├── using-tracedecay/SKILL.md            # bootstrap; injected at session-start on all hosts
│   ├── exploring-code/SKILL.md
│   ├── tracing-functions/SKILL.md
│   ├── assessing-impact/
│   │   ├── SKILL.md
│   │   └── references/                      # support files now shippable (Phase 6)
│   ├── code-health/SKILL.md
│   ├── reviewing-changes/SKILL.md
│   ├── editing-safely/
│   │   ├── SKILL.md
│   │   └── scripts/                         # candidate support dir
│   ├── fixing-build-and-type-errors/
│   │   ├── SKILL.md
│   │   └── references/
│   ├── using-the-cli/
│   │   ├── SKILL.md
│   │   └── references/
│   ├── project-memory/SKILL.md              # MERGED recalling+curating (see §6.1)
│   ├── recalling-session-context/SKILL.md
│   └── inspecting-managed-skills/SKILL.md
│
├── commands/                                # shared slash commands (if any host-neutral ones exist)
├── agents/                                  # shared subagents
│   ├── code-explorer.md
│   ├── code-health-auditor.md
│   └── session-historian.md
├── rules/                                   # Cursor .mdc rules (host-consumed; see overlay note)
│   ├── tracedecay.mdc
│   └── tracedecay-memory.mdc
│
├── hooks/                                   # per-host hook wiring, shared tree
│   ├── hooks-claude.json
│   ├── hooks-cursor.json
│   └── hooks-codex.json
│
├── .claude-plugin/
│   ├── plugin.json                          # → ../skills, ../commands, ../agents, ../hooks/hooks-claude.json
│   └── marketplace.json
├── .cursor-plugin/
│   └── plugin.json                          # → ../skills, ../rules, ../hooks/hooks-cursor.json, mcp
├── .codex-plugin/
│   └── plugin.json                          # → ../skills, ../hooks/hooks-codex.json
│
├── .mcp.json                                # Claude/Codex MCP server config
├── mcp-cursor.json                          # Cursor MCP (was cursor-plugin/mcp.json)
├── mcp-codex.json                           # if Codex needs a distinct file; else reuse .mcp.json
│
├── .cursor/
│   └── commands/                            # Cursor 1.6+ native slash commands (was 13 dispatchers)
│       ├── tracedecay-review-diff.md
│       ├── tracedecay-find-impact.md
│       ├── ... (13 total, see §5.2)
│
└── README.md
```

Deploy target for every host = the composed **plugin root** (superpowers convention). Per-host
installers select which subset of the plugin tree to embed and deploy (§3).

---

## 2. Ordered, dependency-aware phase list

Phases are ordered by dependency. "Atomic" = must land in a single commit/PR or the build/tests break.
"Incremental" = can land host-by-host. **Phase 0 gates everything.**

| Phase | Name | Atomicity | Depends on |
|------|------|-----------|-----------|
| 0 | **Validate manifest-path resolution** (§7.1) | spike, no merge | — |
| 1 | Land `feat/claude-plugin-bundle` as the seed | merge (external) | 0 |
| 2 | Create shared `plugin/skills/` (atomic move) | **ATOMIC** | 1 |
| 3 | Fold Cursor overlays (rules, hooks-cursor, mcp, dispatchers) | incremental | 2 |
| 4 | Fold Codex overlay (hooks-codex, mcp, manifest) | incremental | 2, 3 |
| 5 | Fold Claude overlay (hooks-claude, manifest, marketplace) | incremental | 2 |
| 6 | Recursive-walk embedding + first support files | **ATOMIC** (embed change) | 2 |
| 7 | Add `src/agents/plugin_bundle.rs`; collapse per-host tables | **ATOMIC** | 3,4,5,6 |
| 8 | Steering repoint + universal bootstrap hardening | incremental | 2, 5 |
| 9 | Catalog refinements (memory merge, message_search ownership) | incremental | 2 |
| 10 | Cursor dispatcher → `.cursor/commands/` migration | incremental | 3 |
| 11 | Unified lint + delete parity machinery | **ATOMIC** | 7,9,10 |

**Critical atomic invariant:** the shared `plugin/skills/` move (Phase 2) and the embed-table repoint
that consumes it (early Phase 7 or the coverage-test rewrite) must not straddle a broken intermediate
state. Concretely: when `git mv`ing skills up, update every `include_str!` path **in the same commit**,
or the crate will not compile (`include_str!` is resolved at build time).

---

## 3. Exact Rust changes

### 3.1 New `src/agents/plugin_bundle.rs`
A single module owning the composed file registry, replacing three hand-maintained flat tables.

```rust
//! Canonical shared plugin file set + per-host manifest overlays.
//! Composed view: CANONICAL_PLUGIN_FILES ∪ <HOST>_MANIFEST_FILES.

pub struct PluginFile { pub relative: &'static str, pub contents: &'static str }

/// Shared content: skills/, commands/, agents/, rules/, README — host-neutral.
/// Populated by build-time recursion (see §3.4), NOT hand-listed.
pub const CANONICAL_PLUGIN_FILES: &[PluginFile] = &[ /* generated */ ];

/// Per-host manifest + mcp + host hooks files.
pub const CLAUDE_MANIFEST_FILES: &[PluginFile] = &[ /* .claude-plugin/*, hooks-claude.json, .mcp.json */ ];
pub const CURSOR_MANIFEST_FILES: &[PluginFile] = &[ /* .cursor-plugin/plugin.json, hooks-cursor.json, mcp-cursor.json, rules/*, .cursor/commands/* */ ];
pub const CODEX_MANIFEST_FILES:  &[PluginFile] = &[ /* .codex-plugin/plugin.json, hooks-codex.json, .mcp.json */ ];

/// Composed per-host view consumed by installers.
pub fn claude_files() -> impl Iterator<Item = &'static PluginFile> { /* CANONICAL ∪ CLAUDE_MANIFEST */ }
pub fn cursor_files() -> impl Iterator<Item = &'static PluginFile> { /* CANONICAL ∪ CURSOR_MANIFEST */ }
pub fn codex_files()  -> impl Iterator<Item = &'static PluginFile> { /* CANONICAL ∪ CODEX_MANIFEST */ }
```

Register in `src/agents/mod.rs`.

Design note on host-specific *shared* dirs: `rules/` and `.cursor/commands/` are only meaningful to
Cursor. Keep them physically under `plugin/` (single tree) but attribute them to
`CURSOR_MANIFEST_FILES`, not `CANONICAL`, so Codex/Claude installers do not deploy Cursor rules. The
"canonical" set is exactly the model-invocable `skills/` + shared `agents/` + `README`.

### 3.2 `src/agents/cursor.rs`
- Remove `EMBEDDED_PLUGIN_FILES` (`:199`). Replace its consumers
  (`:390` deploy loop, `:552` path list, `:880` raw lookup, `:1114`/`:1125`/`:1140`/`:1228`/`:1254`
  test helpers) with `plugin_bundle::cursor_files()`.
- Keep the deploy write loop and `collect_regular_files` (`:560`) — they already recurse and will now
  correctly write support subdirs.

### 3.3 `src/agents/codex.rs`
- Remove `CODEX_EMBEDDED_PLUGIN_FILES` (`:252`); route `:546` deploy loop through
  `plugin_bundle::codex_files()`.
- **Delete** `codex_skills_match_the_cursor_source_for_parity` (`:1638`) — parity is meaningless with
  one source of truth.
- Keep `RETIRED_CODEX_PLUGIN_SKILL_DIRS` (`:762`) for uninstall cleanup of legacy dirs.

### 3.4 Recursive embedding (Phase 6, replaces `include_str!` glob gap)
`include_str!` cannot enumerate a directory, so `CANONICAL_PLUGIN_FILES` must be **generated**. Two
acceptable implementations — pick one:
- **(A) `build.rs` codegen (preferred, no new dep):** extend the existing `build.rs` to walk
  `plugin/skills`, `plugin/agents`, `plugin/commands`, `plugin/rules`, `plugin/.cursor/commands`,
  and the manifest dot-dirs, emitting `plugin_bundle_generated.rs` with `PluginFile` entries whose
  `contents` are `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin/…"))`. `include!` it from
  `plugin_bundle.rs`. Gives compile-time embedding + automatic support-file pickup.
- **(B) `include_dir` crate:** `static PLUGIN: Dir = include_dir!("$CARGO_MANIFEST_DIR/plugin");` then
  iterate. Simpler code, adds a dependency; verify license compatibility before choosing.

Either way, the deploy loop stops caring whether a file is `SKILL.md` or `references/foo.md`.

### 3.5 `src/hooks/steering.rs`
- Repoint the bootstrap include: `include_str!("../../cursor-plugin/skills/using-tracedecay/SKILL.md")`
  (`:32`) → `include_str!("../../plugin/skills/using-tracedecay/SKILL.md")`.
- Update `CURSOR_PLUGIN_SKILLS` (`:15`) to the final foundational list after the memory merge (§6.1):
  drop `curating-project-memory` + `recalling-project-memory`, add `project-memory`. Keep the constant
  as the single source the coverage test and steering share (rename to `FOUNDATIONAL_PLUGIN_SKILLS` if
  a Cursor-specific name is now misleading, updating both referencers in one commit).

### 3.6 `src/hooks/claude.rs` and `src/agents/kiro.rs`
- **In-flight — do not re-plan.** A separate agent adds the `using-tracedecay` bootstrap injection to
  `claude_session_context_for_event` (`:126`) and replaces Kiro's "Prefer" `PROMPT_MARKER`
  (`kiro.rs:33`) steering with the shared bootstrap. This plan only requires that once landed, both
  hosts pull the bootstrap body from the **same** `plugin/skills/using-tracedecay/SKILL.md` include
  as steering.rs — a single source of truth (§8).

---

## 4. Exact test changes

### 4.1 Deletions
- `tests/agent_suite/plugin_bundle_sync_test.rs` — **delete entirely** (424 lines). Cross-bundle
  byte-parity is definitionally satisfied by one shared tree. Also drop its registration in
  `tests/agent_suite/main.rs`.
- `src/agents/codex.rs::codex_skills_match_the_cursor_source_for_parity` (`:1638`) — **delete**.

### 4.2 Rewrites
- `src/agents/cursor.rs::embedded_file_list_covers_the_whole_source_bundle` (`:1123`) and
  `src/agents/codex.rs::codex_embedded_file_list_covers_the_whole_source_bundle` (`:1587`) —
  **rewrite** as one coverage assertion over the **composed** `plugin_bundle::<host>_files()` view vs
  the on-disk `plugin/` subset for that host. With the generated registry (§3.4) this becomes a check
  that the walk output equals the deployed set, catching stray/untracked files.
- `tests/agent_suite/skill_lint_cursor_test.rs` — **retarget** the slash-form `/slug` H1 lint (`:152`)
  and the stale-slug reference lint (`:270`) at `plugin/.cursor/commands/*.md`. Model-invocable
  frontmatter checks migrate into the unified contract (§4.3).

### 4.3 New unified lint: `tests/agent_suite/shared_skill_contract_test.rs`
One test over the single `plugin/skills/` tree asserting the **intersection SKILL.md contract** (§5.1),
plus host-extra assertions:
- Every `plugin/skills/*/SKILL.md` passes the intersection contract (frontmatter keys, description
  length/word/trigger rules, single plain-title H1, heading levels, ≤500 lines, LF hygiene).
- Host extras: `.cursor/commands/*.md` carry `disable-model-invocation: true` and matching `/slug` H1;
  `.claude-plugin/marketplace.json` and each `plugin.json` parse and reference existing paths.
- Coverage: the composed embed registry equals the on-disk tree (folds in the rewritten §4.2 checks).
- Replaces `plugin_skill_contract_test.rs` (rewrite in place or supersede + delete).

### 4.4 Keep green
- `tool_skill_coverage` (Design 2): every foundational skill maps to real tracedecay tools. Update in
  lockstep with the `FOUNDATIONAL_PLUGIN_SKILLS` change (§3.5) so it stays green when memory skills
  merge.

---

## 5. Canonical skill format spec + Cursor dispatcher migration

### 5.1 Intersection SKILL.md contract (one file passes Claude + Codex + Cursor)
**Frontmatter**
- Keys ⊆ `{ name, description, allowed-tools, license, metadata }`. No host-specific keys in shared
  skills (Cursor's `disable-model-invocation` lives only under `.cursor/commands/`, §5.2).
- `name`: matches directory name; unique across the set.
- `description`: 50–320 chars, ≤45 words, **trigger-first** (starts with "Use …"), ends with a period,
  no angle brackets, unique across the set.

**Body**
- Exactly **one** H1, plain title form (e.g. `# Exploring code`), never the slash form (`# /foo`).
- No skipped heading levels (H1→H2→H3, never H1→H3).
- **No `## When to Use`** section (trigger lives in the description).
- ≤500 lines.
- LF line endings, exactly one trailing newline.

**Support files** (enabled by Phase 6): a skill may carry `references/`, `scripts/`, `assets/`
subdirs. Codex lint allows `SKILL.md/agents/scripts/references/assets`; keep to that allowlist. Initial
candidates: `editing-safely` (scripts), `fixing-build-and-type-errors` (references), `using-the-cli`
(references), `assessing-impact` (references).

### 5.2 Cursor dispatcher → `.cursor/commands/` migration (Phase 10)
- **Remove** the 13 `tracedecay-*` `disable-model-invocation: true` dispatcher skills from
  `plugin/skills/` so the shared set is one clean model-invocable collection.
- **Re-express** each as a Cursor-native command file under `plugin/.cursor/commands/<slug>.md`
  (Cursor 1.6+). Preserve the `/slug` invocation and body; frontmatter follows Cursor command schema.
- Attribute these files to `CURSOR_MANIFEST_FILES` (§3.1) — Codex/Claude never deploy them.
- Retarget the `/slug` lint (`skill_lint_cursor_test.rs:152`, `:270`) at `.cursor/commands/`.
- **Behavioral parity check:** each new command must invoke the same tracedecay tool sequence its
  old dispatcher did — diff old dispatcher body vs new command body during migration.

---

## 6. Catalog refinements

### 6.1 Merge the two memory skills → `project-memory`
- Today `recalling-project-memory` (read/FTS→fact) and `curating-project-memory` (write/curate) are
  separate; both are in `CURSOR_PLUGIN_SKILLS` (`steering.rs:23`, `:18`). The split forces a pre-read
  adjudication ("which memory skill applies?") the model gets wrong.
- **Merge into one `project-memory` skill** with a **Read** section and a **Curate** section under one
  H1 (respect the single-H1 rule; use H2s). Delete the two source dirs; update
  `FOUNDATIONAL_PLUGIN_SKILLS`, the embed registry, `tool_skill_coverage`, and the unified contract.
- Foundational count moves 13 → 12 (two memory skills become one).

### 6.2 Resolve `message_search` ownership
- **Verified ambiguity:** both `recalling-project-memory/SKILL.md` and
  `recalling-session-context/SKILL.md` reference `tracedecay_message_search`.
- **Assign clear lanes:**
  - `project-memory` (§6.1) owns the **FTS → fact** path (`tracedecay_message_search` used to recall
    durable project facts).
  - `recalling-session-context` owns the **FTS → lcm** path (`tracedecay_message_search` +
    `tracedecay_lcm_expand_query` / `tracedecay_lcm_describe` for compacted session recall).
- Edit both SKILL bodies so each scopes its `message_search` usage to its lane; the unified contract
  can add a soft assertion that the tool is described distinctly (no duplicated verbatim guidance).

### 6.3 Adoption hardening (bootstrap text — single shared body)
The in-flight Claude+Kiro injection work makes bootstrap universal; this plan hardens the **shared**
`plugin/skills/using-tracedecay/SKILL.md` once:
- Add a `<SUBAGENT-STOP>` guard so the bootstrap does not re-fire inside spawned subagents.
- Add an instruction-priority ladder: **defer to user `CLAUDE.md` / `AGENTS.md`** when they conflict.
- Retarget red-flags at the measured failure modes: ~45% of calls are read/body (graph tools unused) —
  explicitly steer toward `tracedecay_context`, `callers`, `callees`, `impact` before native
  read/grep. Because steering.rs, claude.rs, and kiro.rs all pull this one file, the hardening lands
  once and propagates.

---

## 7. Risks and the validation that must happen first

### 7.1 GATING SPIKE — manifest path resolution (Phase 0, do before any move)
**Unknown:** does each host resolve manifest-declared paths **plugin-root-relative** or
**manifest-dir-relative**? Superpowers uses root-relative; if Cursor or Codex resolves relative to the
`.cursor-plugin/` / `.codex-plugin/` dir, the shared-dir `../skills` references break.
- **Validate:** build a throwaway `plugin/` with one host's manifest pointing at `../skills` and
  install into a scratch home for **each** of Claude, Cursor, Codex; confirm skills/commands/hooks
  load. Keep deploy dir = `plugin/` root.
- **Contingency:** if a host is manifest-dir-relative, either (a) use per-host `..`-prefixed paths in
  that manifest, or (b) symlink/duplicate the manifest at the plugin root for that host. Decide before
  Phase 2 — it changes the manifest `plugin.json` contents but **not** the tree layout.

### 7.2 Atomic-compile risk (`include_str!`)
`include_str!` paths resolve at compile time. Any skill move must repoint every include in the **same
commit** (§2 invariant). Mitigation: do Phase 2 as a single `git mv` + include-repoint commit; run
`cargo build` locally before pushing (note: this planning branch does not run cargo; the executor
must).

### 7.3 Support-file embedding must precede shipping support files
Do **not** add a `references/` file to any skill until Phase 6 (recursive embed) lands — otherwise the
file exists on disk, is deployed by the recursive `collect_regular_files` cache walk, but is **not**
embedded, so the coverage test fails and fresh installs miss it. Order: Phase 6 → then support files.

### 7.4 Cursor command-schema drift
`.cursor/commands/` is a Cursor 1.6+ feature. Verify the command frontmatter schema against current
Cursor docs during Phase 10; the 13 dispatchers must keep working for existing users (behavioral
parity check, §5.2).

### 7.5 In-flight coordination
Two branches touch adjacent code: `feat/claude-plugin-bundle` (seed) and the Claude+Kiro bootstrap
agent. Land the seed first (Phase 1); rebase this rearchitecture on top; let the bootstrap work land
independently and only converge on the single-source-of-truth include in Phase 8.

---

## 8. Engineer checklist

**Phase 0 — Gate**
- [ ] Run the manifest-path-resolution spike (§7.1) for Claude, Cursor, Codex. Record which resolution
      mode each uses. Decide manifest path style. **Do not proceed until green.**

**Phase 1 — Seed**
- [ ] Merge `feat/claude-plugin-bundle` to `master`. Rebase this work on the result.
- [ ] Discover the exact Claude embed const name added by that branch.

**Phase 2 — Shared skills (ATOMIC)**
- [ ] `git mv` foundational `skills/*` from cursor-plugin/codex-plugin up into `plugin/skills/`.
- [ ] Repoint **every** `include_str!` to `plugin/skills/...` in the same commit
      (`cursor.rs`, `codex.rs`, claude installer, `steering.rs:32`).
- [ ] `cargo build` locally — must compile before push.

**Phase 3 — Cursor overlay**
- [ ] Move `rules/*.mdc`, `hooks-cursor.json`, `mcp-cursor.json`, `.cursor-plugin/plugin.json`,
      `agents/*` into `plugin/` and attribute to `CURSOR_MANIFEST_FILES`.

**Phase 4 — Codex overlay**
- [ ] Move `hooks-codex.json`, `.mcp.json`, `.codex-plugin/plugin.json`; attribute to
      `CODEX_MANIFEST_FILES`.

**Phase 5 — Claude overlay**
- [ ] Move `.claude-plugin/{plugin.json,marketplace.json}`, `hooks-claude.json`; attribute to
      `CLAUDE_MANIFEST_FILES`.

**Phase 6 — Recursive embed (ATOMIC)**
- [ ] Implement build-time recursive registry (§3.4 option A or B).
- [ ] Confirm deploy still writes exact prior file set (no regressions), then add first support-file
      subdir (start with one, e.g. `using-the-cli/references/`).

**Phase 7 — plugin_bundle.rs (ATOMIC)**
- [ ] Add `src/agents/plugin_bundle.rs`; register in `mod.rs`.
- [ ] Route `cursor.rs`/`codex.rs`/claude installer deploy loops through `<host>_files()`.
- [ ] Delete the three flat `include_str!` tables.

**Phase 8 — Steering + bootstrap**
- [ ] Point `steering.rs` bootstrap include at `plugin/skills/using-tracedecay/SKILL.md`.
- [ ] Update `CURSOR_PLUGIN_SKILLS` → `FOUNDATIONAL_PLUGIN_SKILLS` (post-merge list).
- [ ] Harden shared bootstrap text (`<SUBAGENT-STOP>`, priority ladder, retargeted red-flags) — once,
      in the shared file. Confirm claude.rs/kiro.rs (in-flight) consume the same include.

**Phase 9 — Catalog**
- [ ] Merge `recalling-project-memory` + `curating-project-memory` → `project-memory` (Read/Curate H2s).
- [ ] Assign `message_search` lanes (project-memory = FTS→fact; recalling-session-context = FTS→lcm);
      edit both bodies.
- [ ] Update `FOUNDATIONAL_PLUGIN_SKILLS`, embed registry, `tool_skill_coverage`, unified contract.

**Phase 10 — Cursor commands**
- [ ] Move 13 `tracedecay-*` dispatchers from `skills/` to `plugin/.cursor/commands/`.
- [ ] Diff each old dispatcher vs new command for tracedecay-tool parity.
- [ ] Retarget `/slug` lints at `.cursor/commands/`.

**Phase 11 — Lint + cleanup (ATOMIC)**
- [ ] Add `shared_skill_contract_test.rs` (intersection contract + host extras + composed coverage).
- [ ] Delete `plugin_bundle_sync_test.rs` and its `main.rs` registration.
- [ ] Delete `codex_skills_match_the_cursor_source_for_parity`.
- [ ] Rewrite the two `embedded_file_list_covers_*` unit tests over the composed view.
- [ ] Keep `tool_skill_coverage` green.
- [ ] Full `cargo test` on the agent suite.

**Final**
- [ ] Delete the now-empty `cursor-plugin/` and `codex-plugin/` trees (and `claude-plugin/` if the seed
      created a separate one) once `plugin/` is authoritative.
- [ ] Update any repo docs / README references to the old bundle paths.

---

## 9. Summary of what changes and why (one-paragraph orientation)

Three duplicated host bundles collapse into one `plugin/` tree deployed root-relative per host, with
per-host manifest dot-dirs selecting a subset via `src/agents/plugin_bundle.rs` (`CANONICAL` ∪
`<HOST>_MANIFEST`). The Rust embed layer moves from three hand-maintained flat `include_str!` tables to
one build-time recursive registry, which finally lets skills ship `references/`/`scripts/`/`assets/`.
The skill catalog becomes one clean model-invocable set (13→12 after the memory merge) with Cursor's
slash dispatchers re-expressed as native `.cursor/commands/`. The test surface drops cross-bundle
parity entirely and gains one unified intersection-contract lint. The `using-tracedecay` bootstrap is
sourced from a single shared file injected at session-start on every host. The one hard gate is the
manifest-path-resolution spike (§7.1) — run it before moving anything.
```
