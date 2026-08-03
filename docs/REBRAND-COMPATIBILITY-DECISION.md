# TraceDecay rebrand compatibility — root triage decision

Date: 2026-06-14
Task: `t_cc48f585` (root synthesis for `t_3482e439`, `t_4070b0b0`, `t_da7151b6`, `t_a33b4ad3`)
Status: **Policy adopted as canonical.** This document ratifies it and records the
root-level verification, the one decision that still needs a human, and the
resolved open question.

> **Historical triage record — not implementation authority.** The policy remains
> the current authority for independently released public interfaces,
> installed-agent artifacts, and external user-owned data. This record does not
> authorize a V2 internal-store reader, converter, backfill, dual write, profile
> census, staged cutover, or branch/worktree fact migration.

This is the capstone for the four child artifacts. It does not restate them; it
points at them and records what the root reviewer verified and decided.

## Artifacts produced by this triage

| Artifact | Role | Child task |
|---|---|---|
| `docs/TRACEDECAY-COMPATIBILITY-AUDIT.md` | Fact inventory of every legacy surface, with file:line anchors and risk notes. | `t_3482e439` |
| `docs/TREESITTERS-RENAME-CONSTRAINTS.md` | Deep dive on the one externally constrained name. | `t_4070b0b0` |
| `docs/REBRAND-COMPATIBILITY-POLICY.md` | Normative policy: categories A–E, principles, a 37-row surface map, and a review checklist. | `t_da7151b6` |
| `docs/REBRAND-COMPATIBILITY-FOLLOW-UP-CHECKLIST.md` | Retired ticket backlog; current review guardrails for released external compatibility surfaces. | `t_a33b4ad3` |

## Answer to the triage question

The task asked the root to document *what remains indefinitely, what migrates
automatically, what warns, and what must never be renamed yet due to upstream
dependency constraints.* Mapping the policy's 37 audited surfaces onto its five
categories gives the one-line answer:

| Triage question | Policy category | Surfaces | Summary |
|---|---|---:|---|
| Remains indefinitely | A — retained in place | 11 | Release-evidenced external user data, discovery, counter endpoint, and historical docs/changelog/benchmarks. No automatic V2 internal-state conversion. |
| Reconciles owned artifacts | B — installers/refreshers reconcile | 16 | Generated agent/plugin config keys, prompt/rule markers, plugin dirs, marketplace entries, released Hermes aliases, release-asset probing for explicit old versions, primary-doc wording. |
| Warns (accepted as fallback) | C — accepted, new spelling wins | 8 | `TRACEDECAY_*` env fallbacks, `DISABLE_TRACEDECAY`, global-DB toggles, hook/extraction/pricing vars, plugin-path docs claim, Homebrew/Scoop package note. |
| Never silently accepted | D — reject/fail | 1 | Legacy-only / pre-reset releases in automatic latest-version detection. |
| Never renamed yet (upstream constraint) | E — externally owned | 1 | `tracedecay-large-treesitters` (+ `-medium`/`-lite`). Keep until upstream renames all three or a maintained fork is approved as a policy change. |

Categories A and E are the load-bearing "do not touch" answer; B and C are the
"acceptable legacy debt, managed deliberately" answer; D is the supply-chain
guardrail. No surface is left uncategorized.

## Root verification (not just trusting the child reports)

Spot-checked the load-bearing claims against the working tree — all accurate:

- `tracedecay-large-treesitters` is declared as an upstream git dep at
  `Cargo.toml:108` (`aovestdipaperino/tracedecay-large-treesitters`, `0.5.0`), and
  the three import sites match the constraints doc exactly:
  `tracedecay_large_treesitters::all_languages()` at `src/extraction/ts_provider.rs:27`,
  and `markdown::inline::LANGUAGE` / `markdown::LANGUAGE` at
  `src/extraction/markdown_extractor.rs:70,139`.
- The no-fork / upstream policy the constraints doc relies on is present at
  `AGENTS.md:20`.
- The in-place legacy storage contract is explicit in code:
  `src/config.rs:16-24` defines `LEGACY_TRACEDECAY_DIR = ".tracedecay"` and
  `LEGACY_DB_FILENAME = "tracedecay.db"` with a "no auto-migration" comment.
- `DISABLE_TRACEDECAY=true` is honored as an exact-string `true` serve opt-out
  alongside `DISABLE_TRACEDECAY=true` (`src/main.rs:1020-1024`). *(Minor line
  drift: the audit cited `main.rs:1150-1157`; in this checkout it is at
  `1020-1024`. Behavior is exactly as documented — non-material.)*

One child-flagged open question resolved by the root review (closes M-04/T-03 as
a docs-only fix):

- `$TRACEDECAY_PLUGIN_PATH` / `$TRACEDECAY_PLUGIN_PATH` and `.tracedecay/plugins/`
  discovery is documented at `docs/PLUGINS-DESIGN.md:138-156`, but **no source in
  `src/` reads these variables**. The whole "Plugin discovery" section there
  describes an aspirational dynamic-`.so`-plugin loader (env path, project/user
  platform dirs, `plugin.toml` manifests) that is not implemented. So this is not
  a missing-test gap or a runtime regression — it is a docs claim that
  over-promises behavior that was never built. **Decision: downgrade
  PLUGINS-DESIGN.md to mark the plugin-discovery mechanism (including its legacy
  fallbacks) as a future compatibility target, not a guaranteed runtime
  behavior.** The policy's Category C wording for this surface already permits
  exactly that, so no policy edit is needed — only the design-doc edit in the
  checklist item M-04.

## Open decisions that still need a human

Only one item in the four artifacts is a genuine fork that cannot be resolved by
a worker and is not pure implementation work:

- **W-04 — `DISABLE_TRACEDECAY` boolean semantics.** Today `DISABLE_TRACEDECAY`
  matches the exact string `true` (`src/main.rs:1020-1021`), while the generic
  `brand_env()` boolean consumers parse truthy values (`1/true/yes/on`). The
  policy correctly refuses to pick a side without input.
  - Root recommendation: **keep exact-string `true`** for the disable/opt-out
    vars specifically (they are kill-switches where strict matching avoids a
    typo'd `tru` silently disabling the server), and document the intentional
    divergence in tests. Truthy parsing stays the default for feature-toggle
    vars. Confirm or override before the W-04 card is implemented.

The former follow-up ticket list is retired. New work must be scoped against
current release evidence and the policy; source-only V2 shapes change in place.

## Current review pointer

Use `docs/REBRAND-COMPATIBILITY-FOLLOW-UP-CHECKLIST.md` as a compact review
guardrail, not a delivery queue. It preserves only externally released public
compatibility, owned generated artifacts, truthful documentation, and explicit
external user-data safety. It does not create a V2 store-conversion or
branch/worktree fact-migration path.
