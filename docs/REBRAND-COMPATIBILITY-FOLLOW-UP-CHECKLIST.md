# TraceDecay rebrand compatibility follow-up checklist

Date: 2026-06-14
Sources: `docs/REBRAND-COMPATIBILITY-POLICY.md`, `docs/TOKENSAVE-COMPATIBILITY-AUDIT.md`, `docs/TREESITTERS-RENAME-CONSTRAINTS.md`

Goal: turn the approved compatibility policy into small, implementation-ready follow-up tasks without making code changes in this document.

## How to use this checklist

- Each item below is intentionally small enough to become its own Kanban card.
- "Touchpoints" name the primary files to inspect or change first.
- "Done when" states the minimum acceptance criteria for the future implementation task.
- Items under "Do not change" are explicit guardrails: they should stay as-is unless policy changes.

## 1. Warning plumbing for legacy runtime inputs

### W-01: Add a shared legacy-name warning helper for env/config fallbacks
- Area: warning messages
- Touchpoints:
  - `src/config.rs`
  - `src/global_db.rs`
  - `src/dashboard/savings_pricing.rs`
  - any shared logging/warning utility chosen by the implementer
- Why: the policy requires concise warnings when legacy `TOKENSAVE_*` spellings are honored, but current fallback helpers (`brand_env`, `env_with_legacy`) silently accept old names.
- Done when:
  - there is one reusable helper for "old name honored" and "both old and new set; new wins"
  - warnings never print secret values
  - warnings are emitted at most once per key per process or invocation

### W-02: Wire warnings into the generic `TRACEDECAY_*` / `TOKENSAVE_*` fallback path
- Area: warning messages
- Touchpoints:
  - `src/config.rs` (`brand_env`)
  - consumers in `src/hooks.rs`, `src/tracedecay.rs`, `src/global_db.rs`
- Why: most legacy env compatibility flows through `brand_env`, so this is the highest-leverage place to enforce Category C behavior.
- Done when:
  - old-only env usage warns once
  - both-set precedence warns once and uses `TRACEDECAY_*`
  - new-only usage stays silent

### W-03: Add warnings for savings-pricing legacy env fallbacks
- Area: warning messages
- Touchpoints:
  - `src/dashboard/savings_pricing.rs`
- Why: pricing uses its own `env_with_legacy` helper instead of `brand_env`, so it will miss any shared warning work unless updated separately.
- Done when:
  - `TOKENSAVE_OFFLINE` and `TOKENSAVE_MODEL_PRICES_PATH` behave like other Category C fallbacks
  - logs mention keys only, not paths or values beyond what policy permits

### W-04: Decide and codify `DISABLE_TOKENSAVE` boolean semantics
- Area: warning messages / env semantics
- Touchpoints:
  - `src/main.rs`
  - `README.md`
  - `docs/dashboard.md`
  - new tests near current serve opt-out coverage
- Why: `DISABLE_TOKENSAVE` is currently exact-string `true`, while most other boolean fallbacks use truthy parsing. The policy says not to change semantics casually.
- Done when:
  - the project explicitly chooses either "keep exact-string behavior" or "normalize to shared truthy parsing"
  - docs and tests match that choice
  - legacy opt-out still exits `tracedecay serve` cleanly

## 2. Migration helpers and alias handling

### M-01: Verify Hermes pinned-project-root alias migration end-to-end
- Area: env/config alias handling
- Touchpoints:
  - `src/agents/hermes/profile_config.rs`
  - `src/agents/hermes/tokensave_migration.rs`
  - `src/agents/hermes/lifecycle.rs`
- Why: the policy requires `plugins.tokensave.project_root` to migrate cleanly to `plugins.tracedecay.project_root` when no current key exists.
- Done when:
  - reinstall/refresh preserves a legacy pin when appropriate
  - a current `plugins.tracedecay.project_root` still wins over the old key
  - uninstall/cleanup removes generated legacy artifacts without deleting unknown user files

### M-02: Verify Hermes plugin/memory/context alias rewrites
- Area: env/config alias handling
- Touchpoints:
  - `src/agents/hermes/profile_config.rs`
- Why: `plugins: ["tokensave"]`, `provider: tokensave`, and `engine: tokensave` are all Category B aliases that should be rewritten to canonical `tracedecay` behavior.
- Done when:
  - enable/disable flows handle both old and new spellings predictably
  - conflicting non-tracedecay providers/engines still fail closed instead of being overwritten silently
  - tests cover plugin list, memory provider, and context engine separately

### M-03: Audit agent integrations for missing legacy cleanup coverage
- Area: migration helpers
- Touchpoints:
  - `src/agents/codex.rs`
  - `src/agents/claude.rs`
  - `src/agents/antigravity.rs`
  - `src/agents/copilot.rs`
  - `src/agents/{cline,gemini,kilo,kimi,kiro,opencode,roo_code,vibe,zed}.rs`
  - `tests/agent_test.rs`
  - `tests/claude_agent_test.rs`
- Why: the audit found strong legacy-specific tests for Cursor and Hermes, but weaker or unclear coverage for other integrations that also claim to remove `tokensave` artifacts.
- Done when:
  - each owned integration has at least one focused test proving legacy `tokensave` entries are reconciled or removed
  - tests distinguish generated artifacts from user-authored files
  - uninstall and reinstall both stay idempotent

### M-04: Resolve the plugin-path fallback docs/implementation mismatch
- Area: env/config alias handling
- Touchpoints:
  - `docs/PLUGINS-DESIGN.md`
  - plugin discovery implementation, if it exists
- Why: docs currently claim `$TOKENSAVE_PLUGIN_PATH` and `.tokensave/plugins/` fallbacks, but the audit did not find implementation.
- Done when:
  - either implementation is located and covered by tests, or
  - the docs are downgraded to a compatibility target / follow-up instead of a guaranteed runtime behavior
  - the policy wording and docs agree

## 3. Docs alignment work

### D-01: Update active docs to present TraceDecay names first and compatibility second
- Area: docs edits
- Touchpoints:
  - `README.md`
  - `docs/USER-GUIDE.md`
  - `docs/dashboard.md`
  - `docs/LSP-INTEGRATION.md`
  - `SECURITY.md`
- Why: active docs should use TraceDecay as canonical naming while keeping short compatibility notes where runtime fallback still exists.
- Done when:
  - examples default to `tracedecay`, `.tracedecay`, and `TRACEDECAY_*`
  - any remaining `tokensave` mentions are clearly historical, compatibility-related, or external
  - daemon/service cleanup notes for old installs remain intact where still useful

### D-02: Add explicit compatibility-note wording for supported legacy env/path fallbacks
- Area: docs edits
- Touchpoints:
  - `README.md`
  - `docs/dashboard.md`
  - any troubleshooting docs that mention env overrides
- Why: after warning behavior is implemented, the docs should tell users what still works, what warns, and what wins when both names are set.
- Done when:
  - docs consistently say legacy names are accepted as fallbacks
  - docs say new names win on conflicts
  - docs do not over-promise unverified plugin-path behavior

### D-03: Link the policy into contributor review paths
- Area: docs edits / process
- Touchpoints:
  - `AGENTS.md`
  - contributor-facing docs or PR templates, if the repo keeps one
- Why: the policy includes a review checklist, but future PRs touching `tokensave` surfaces will drift unless reviewers can find the policy quickly.
- Done when:
  - contributor guidance points rebrand-related changes to `docs/REBRAND-COMPATIBILITY-POLICY.md`
  - reviewers have a short reminder to classify each touched surface into policy category A-E

## 4. Test follow-ups

### T-01: Add direct tests for legacy env warning behavior
- Area: tests
- Touchpoints:
  - tests adjacent to `src/config.rs`, `src/global_db.rs`, `src/dashboard/savings_pricing.rs`, `src/main.rs`
- Why: the policy requires old-only, new-only, and both-set precedence coverage once warnings exist.
- Done when:
  - tests cover old-only, new-only, both-set, and warning/no-warning cases
  - tests assert no secret values are printed

### T-02: Add focused tests for non-Cursor agent legacy cleanup
- Area: tests
- Touchpoints:
  - `tests/agent_test.rs`
  - `tests/claude_agent_test.rs`
- Why: the audit explicitly called out missing or unclear legacy-specific tests for Codex, Antigravity, Claude, Copilot, Kimi/Kilo/Roo/Cline/Gemini/Zed/OpenCode/Vibe.
- Done when:
  - there is at least one regression test per integration family with legacy `tokensave` config/artifacts
  - owned files are removed/reconciled, unknown user files preserved

### T-03: Add a regression test for plugin-path fallback resolution or remove the docs claim
- Area: tests
- Touchpoints:
  - plugin discovery tests, if implementation exists
  - otherwise docs-only change under `docs/PLUGINS-DESIGN.md`
- Why: this is the most obvious gap between documented and verified behavior.
- Done when:
  - the project either has a real test-backed fallback, or the docs stop claiming it already exists

### T-04: Preserve current no-auto-migration storage behavior with explicit regression coverage
- Area: tests
- Touchpoints:
  - `src/config.rs`
  - `src/global.rs`
  - `src/branch_meta.rs`
  - any maintenance-command tests covering discovery/wipe previews
- Why: the policy treats silent storage renames as forbidden. Existing tests cover parts of this, but future follow-up work should keep the no-migration contract visible.
- Done when:
  - legacy `.tokensave/` and `~/.tokensave/` usage remains in-place when it is the active data root
  - `.tracedecay/` still wins when both exist
  - maintenance/discovery flows continue to surface legacy project roots

## 5. Explicit do-not-change items

These are intentional non-goals unless the policy itself changes.

### N-01: Do not auto-rename `.tokensave/`, `~/.tokensave/`, or `tokensave.db`
- Keep existing legacy project/user data roots active in place.
- Any future migration must be explicit, backup-first, atomic, and reversible.
- Primary touchpoints to leave behaviorally unchanged:
  - `src/config.rs`
  - `src/global.rs`
  - `src/branch_meta.rs`
  - `src/dashboard/curate_preview_store.rs`
  - `src/diagnostics/rust.rs`

### N-02: Do not remove legacy maintenance discovery
- Keep recognizing `.tokensave/tokensave.db` in list/status/wipe-style maintenance flows.
- This is compatibility, not historical fluff.

### N-03: Do not rename or fork `tokensave-large-treesitters`
- Keep the exact upstream dependency names:
  - `tokensave-large-treesitters`
  - `tokensave-medium-treesitters`
  - `tokensave-lite-treesitters`
- Constraints are documented in `docs/TREESITTERS-RENAME-CONSTRAINTS.md`.
- Only revisit if upstream renames all three crates or the project explicitly approves a maintained fork.

### N-04: Do not rename the worldwide counter endpoint yet
- Keep `tokensave-counter` until a replacement endpoint is deployed with preserved continuity/behavior.
- Touchpoints:
  - `src/cloud.rs`
  - `SECURITY.md`

### N-05: Do not rewrite historical artifacts just to remove old names
- Leave changelog history, old plans/specs, benchmark outputs, and `docs/TOKEN-SAVE-WHATSNEW.md` intact except for factual corrections.
- This is archival history, not active product naming.

## Suggested implementation order

1. `W-01` + `W-02` + `T-01` — shared warning infrastructure first.
2. `W-03` + `W-04` — finish the env warning/semantics edge cases.
3. `M-01` + `M-02` — Hermes alias and migration hardening.
4. `M-03` + `T-02` — agent cleanup coverage expansion.
5. `M-04` + `T-03` — resolve plugin-path doc/implementation mismatch.
6. `D-01` + `D-02` + `D-03` — align active docs after runtime behavior is settled.
7. `T-04` — reinforce the no-auto-migration guardrails.

## Notes for future task authors

- Use the policy categories in `docs/REBRAND-COMPATIBILITY-POLICY.md` when scoping each implementation card.
- Keep ordinary schema/config compatibility separate from TokenSave-brand compatibility.
- Treat any task that touches legacy storage paths or deletes old files as higher risk than wording-only doc edits.
