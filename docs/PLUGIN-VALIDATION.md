# Plugin and Skill Validation

> **Layout note (single-bundle rearchitecture):** the three duplicated bundles
> `cursor-plugin/`, `codex-plugin/`, and `claude-plugin/` have been collapsed
> into one shared `plugin/` tree. Shared skills live in `plugin/skills/`;
> per-host manifests live in `plugin/.cursor-plugin/`, `plugin/.codex-plugin/`,
> `plugin/.claude-plugin/`; per-host hooks are `plugin/hooks/hooks-<host>.json`;
> MCP configs are `plugin/.mcp.json` (Claude/Codex) and `plugin/mcp-cursor.json`
> (Cursor, deployed as `mcp.json`); READMEs are `plugin/README-<host>.md`; and
> Cursor's 13 workflow slugs ship as native Cursor 1.6+ slash commands
> (`plugin/overlays/cursor/commands/*.md`, deployed to `commands/` and declared
> by the manifest's `commands` key), *not* as `disable-model-invocation`
> dispatcher skills. Cursor's shared skill set is therefore the 17 canonical
> model-invocable skills, byte-identical to Claude/Codex. The composed per-host
> deploy set is owned by `src/agents/plugin_bundle.rs`. Sections below that
> describe cross-bundle *parity/mirroring* are historical — with one shared tree
> there is nothing to keep in sync.

How the bundled agent plugins (shared `plugin/` tree) and their
skills are validated, where each check runs, and how to extend the system
without breaking the contracts.

This document covers the validation of the *agent integration bundles* — the
Cursor plugin, the Codex plugin, and their skills, hooks, rules, and MCP
registrations. It is unrelated to the language-extractor plugin runtime
described in [`PLUGINS-DESIGN.md`](PLUGINS-DESIGN.md).

---

## Why this exists

The plugin bundles are consumed by external hosts (Cursor, Codex, Claude Code)
whose loaders are strict and whose failure mode is usually *silent*: a manifest
key with a typo, a skill frontmatter field the host doesn't allow, or an MCP
config that doesn't match the host's schema simply causes the component to not
load. Nothing in `cargo build` catches that. The validation layers below turn
those silent failures into test failures.

---

## Validation layers

The checks form layers, from cheapest to most end-to-end. Layers 1–5 run
locally under `cargo nextest run` (and therefore also in CI's normal test
job); layer 6 covers what can't live in the Rust test harness.

### 1. Schema validation (cargo test)

JSON artifacts in the bundles are validated against vendored JSON Schemas in
`tests/fixtures/cursor-schemas/`:

| Artifact | Schema | Test |
|---|---|---|
| `plugin/.cursor-plugin/plugin.json` | `plugin.schema.json` | `tests/agent_suite/plugin_manifest_schema_test.rs` |
| `plugin/.codex-plugin/plugin.json` | `plugin.schema.json` + `interface` extension | `tests/agent_suite/plugin_manifest_schema_test.rs` |
| `plugin/.claude-plugin/marketplace.json` | `marketplace.schema.json` | vendored for refresh parity |
| `plugin/mcp-cursor.json` (deploys as `mcp.json`) | `mcp.schema.json` | `tests/agent_suite/plugin_config_schema_test.rs` |
| `plugin/hooks/hooks-cursor.json` and `plugin/hooks/hooks-codex.json` | `hooks.schema.json` | `tests/agent_suite/plugin_config_schema_test.rs` |

The tests use the `jsonschema` crate (dev-dependency only, no network
resolvers — the schemas are self-contained draft-07, and the shipped binary
never validates schemas at runtime).

Beyond schema shape, `tests/agent_suite/plugin_manifest_schema_test.rs` also asserts that
every component path a manifest declares (`skills/`, `hooks/hooks.json`,
`rules/*.mdc`, …) resolves to a real file or directory in the bundle, and
that both bundles share the same plugin `name`. The config-schema tests
include negative cases proving the mcp/hooks schemas actually reject
malformed configs (missing `command`, unknown fields, typo'd event names).

The Cursor plugin schema declares `additionalProperties: false`, and Codex
marketplaces read an `interface` display-metadata block that Cursor's schema
doesn't define. The Codex manifest is therefore validated against the Cursor
schema plus exactly that one extra key, derived in the test.

### 2. Skill contract tests (cargo test)

`tests/agent_suite/plugin_skill_contract_test.rs` enforces the per-host skill
contract over every `SKILL.md` in both bundles:

- **Frontmatter allowlists per host.** Codex skills may only use the keys
  accepted by Codex's `quick_validate.py` (`name`, `description`,
  `allowed-tools`, `license`, `metadata`); Cursor skills additionally allow
  `disable-model-invocation`. `name` and `description` are required
  everywhere.
- **Size budgets.** Skill bodies stay under 500 lines; descriptions stay under
  320 characters / 45 words; a bundle's total preloaded name+description
  metadata stays under 6,000 characters so skill discovery never crowds the
  host's context window.
- **Trigger-first descriptions.** Every description must contain trigger
  language ("Use when …"), because hosts only show agents the metadata before
  the body is loaded. A body-only "When to Use" section is rejected.
- **Supported resource layout.** A skill directory may only contain
  `SKILL.md` plus the supported resource directories (`agents/`, `scripts/`,
  `references/`, `assets/`).
- **Byte-copy install parity.** Installing the Cursor or Codex integration
  into a temp home must produce a byte-identical copy of the source skill
  tree — this catches install-time mutation and missing `include_str!`
  registrations (see [Adding a skill](#adding-a-skill-correctly)).

On top of the shared contract, `tests/agent_suite/skill_lint_cursor_test.rs` lints the
Cursor bundle with rules adapted from community SKILL.md linters (skillmark,
skilldoctor, skillkit) and Cursor's skills docs:

- **File hygiene:** no BOM, CRLF, tabs, or trailing whitespace; exactly one
  trailing newline; balanced code fences; no placeholder text; non-empty
  body.
- **Heading conventions:** exactly one H1; no skipped heading levels;
  model-invocable skills use a plain-title H1 (never the slash form). The
  Cursor native commands (`plugin/overlays/cursor/commands/*.md`) are linted
  separately: each must open with a `# /slug` H1 matching its file name and
  reference only bundled skills and live MCP tools.
- **Name/description quality:** no reserved `claude`/`anthropic` prefixes;
  descriptions ≥ 50 chars, unique across the bundle, ending in terminal
  punctuation, with no angle brackets.
- **Reference resolution:** relative links, bundled-resource mentions,
  `tracedecay:<slug>` cross-skill references, backticked `/slug` invocations,
  and every `tracedecay_<name>` tool identifier must resolve (tool names are
  checked against the live `tracedecay::mcp::get_tool_definitions()` list);
  `paths` globs must be relative, forward-slash, without `..`.

### 3. Cross-bundle sync (cargo test)

`cursor-plugin/` is the **source of truth**. The Codex bundle is a mirror of
the Cursor skills, embedded via `include_str!` in `src/agents/codex.rs` and
checked by the unit test `codex_skills_match_the_cursor_source_for_parity`:
every model-invocable Cursor skill (the `hooks::CURSOR_PLUGIN_SKILLS` list in
`src/hooks.rs`) must exist in the Codex bundle, and content divergence is
only allowed through explicit per-skill allowlists in that test. Cursor-only
skills — the `tracedecay-*` slash dispatchers — are exempt from mirroring.

Practical consequence: **never edit a `codex-plugin/skills/*/SKILL.md` by
hand.** Edit the Cursor source and propagate, or the parity test fails.

On top of the skill-level parity, `tests/agent_suite/plugin_bundle_sync_test.rs` enforces
disk-level cross-bundle sync through three declarative tables: the bundle
list, a top-level manifest assigning every bundle entry a policy
(`SyncedSkills`, `HostSpecific { reason }`, or `OnlyIn { bundles, reason }`),
and a skill exception table (`OnlyIn`, `DivergentBody`,
`DivergentFrontmatter`). The default is strict — every skill ships in every
bundle with a byte-identical tree — and any deviation needs a documented
exception. The tables are self-cleaning: an undeclared divergence fails, and
so does a *stale* exception (an `OnlyIn` that no longer matches, or a
declared divergence that no longer diverges). The exception table mirrors the
codex.rs allowlists — if the two drift apart, one of the tests fails and
names the other — and the set of skills shared by every bundle must equal
`hooks::CURSOR_PLUGIN_SKILLS`. The assertions are bundle-count agnostic: a
future ecosystem bundle joins the check by adding one `Bundle` row plus its
manifest and exception entries.

### 4. Rendered-output and manifest-path validation (cargo test)

Beyond validating the *source* bundles, install-time output is validated.

**Manifest location.** Cursor requires the manifest at
`.cursor-plugin/plugin.json` inside the plugin root (per
[cursor.com/docs/plugins](https://cursor.com/docs/plugins) and the official
[cursor/plugins](https://github.com/cursor/plugins) marketplace repo). This
repo already conforms — `cursor-plugin/.cursor-plugin/plugin.json` in source,
and `src/agents/cursor.rs` renders it to
`~/.cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json`. The layout is
pinned by existing assertions in `tests/agent_suite/agent_test.rs` and
`tests/agent_suite/update_plugin_test.rs`. Note that plain `ls` hides the
dot-directory; use `ls -a` before concluding a bundle has no manifest.

**Rendered output.** Installers stamp `CARGO_PKG_VERSION` into the manifest
and rewrite MCP/hook commands to the resolved absolute `tracedecay` binary
path; source bundles keep `"version": "0.0.0"` and a bare `tracedecay`
command. Rendered-output validation in
`tests/agent_suite/update_plugin_test.rs`
(`cursor_install_renders_structurally_valid_bundle` and friends) installs
into a temp home and checks the rendered artifacts: the rendered manifest is
validated against the vendored plugin schema (full draft-07 validation, same
`jsonschema` crate as layer 1), every source file appears in the rendered install,
no unresolved `${...}` placeholders survive in rendered JSON (except the
intentional `${workspaceFolder}` MCP arg), and hook/MCP commands reference
the shell-quoted absolute binary path. This complements the byte-copy skill
parity in layer 2.

### 5. Claude Code portability (cargo test)

`tests/agent_suite/skill_lint_claude_test.rs` lints every skill in both bundles against
Claude Code / Agent Skills portability rules, so a future `claude-plugin/`
bundle would be a re-packaging exercise rather than a rewrite. The rules
(sources cited in the test's module docs) include: frontmatter keys limited
to Claude-Code-documented fields, kebab-case `name` matching the directory
(≤ 64 chars, no XML tags, no reserved words `anthropic`/`claude`),
`description` non-empty with no angle brackets (≤ 1,024 chars, and
`description` + `when_to_use` ≤ 1,536 chars — Claude Code truncates listings
beyond that), and the shared 6,000-char per-bundle metadata budget.

One Cursor-required field conflicts with the strict Agent Skills open spec —
`disable-model-invocation`. Claude Code itself supports it, so it is a
*documented skip* (`CROSS_ECOSYSTEM_CONFLICT_FIELDS` in the test), with a
stale-allowlist guard that fails if a documented conflict field stops being
used. Any *new* nonconformant field fails the strict-spec test, and the Codex
bundle must stay 100% spec-clean.

### 6. Checks outside the Rust test harness

Most validation deliberately lives in `cargo test`, because the existing
`ci.yml` `test` job already runs the full suite on every PR — a check that can
be a `#[test]` needs no new YAML. CI-only additions are limited to what cargo
can't do:

- **Schema-validation workflow**
  (`.github/workflows/plugin-validation.yml`, `manifest-schema` job). Mirrors
  the official `cursor/plugins` marketplace validation: `ajv` compiles all
  four vendored schemas (so a broken schema edit fails even when no manifest
  changed), validates the Cursor manifest against `plugin.schema.json`, and
  parse-checks every `*.json` in both bundles. The Codex manifest is only
  parse-checked here (its layout differs from Cursor's); its semantics are
  covered by the Rust tests. The workflow is path-filtered to bundle, schema,
  and plugin-test paths, so it shows as *skipped* on unrelated PRs — account
  for that before making it a required check.
- **MCP conformance smoke** (`scripts/mcp-conformance-smoke.sh`, run in CI by
  the `mcp-conformance-smoke` job of the same workflow). Drives a real
  `tracedecay serve` process through the official MCP Inspector CLI
  (pinned version, warm-cache offline), which embeds the official TypeScript
  SDK client. This adds what the Rust `tests/mcp_suite/` cannot: a
  protocol-version-negotiation handshake with a newer client, SDK-side (Zod)
  shape validation of capabilities and every tool's `inputSchema`, and the
  real client lifecycle ordering. Seven checks run against a hermetic
  throwaway fixture project (redirected HOME, ~6 s warm). It needs a built
  binary and npx, which is why it isn't a plain `#[test]`. Run it directly,
  or with `TRACEDECAY_BIN=target/debug/tracedecay` to pin the binary. The
  official `@modelcontextprotocol/conformance` suite was evaluated and
  rejected for now — it only connects over streamable HTTP and
  `tracedecay serve` is stdio-only; revisit if an HTTP transport lands.

---

## Vendored schemas: provenance and refresh

The Cursor schemas live in `tests/fixtures/cursor-schemas/`. They are
*vendored*, not fetched at test time: tests must pass offline and must not
break when an upstream URL moves. Each schema carries an `$id` recording the
upstream identity it mirrors (e.g.
`https://cursor.com/schemas/cursor-plugin/plugin.json`).

The four schemas have two kinds of provenance:

- **`plugin.schema.json` and `marketplace.schema.json`** are copies from the
  official [cursor/plugins](https://github.com/cursor/plugins) marketplace
  repository, which publishes them and validates its own plugins against them
  in CI. Refresh by diffing against the current upstream copy and recording
  the upstream commit hash in your commit message.
- **`mcp.schema.json` and `hooks.schema.json`** are *derived*, not copied:
  Cursor publishes no standalone machine-readable schema for `mcp.json` or
  `hooks.json` (the official plugin schema types those inline fields as bare
  objects). They were written from Cursor's field references at
  [cursor.com/docs/context/mcp](https://cursor.com/docs/context/mcp) and
  [cursor.com/docs/hooks](https://cursor.com/docs/hooks), cross-checked
  against the hooks configs shipped by official plugins in `cursor/plugins`.
  Each schema's top-level `description` records the derivation details and
  date. Hook event names are enumerated from the documented list, so a Cursor
  release that adds new events requires re-vendoring.

After any refresh, run the schema tests; if the bundles no longer validate,
fix the bundles in the same change — a schema refresh that breaks the shipped
manifests is a real finding, not test noise. (Exactly this happened when the
schemas were first vendored: the manifests carried an `author.url` key the
official schema rejects.)

---

## Adding a skill correctly

There is now one shared skill tree; there is no per-bundle mirroring to
maintain, and skill files are embedded **recursively** by `build.rs` — you do
not hand-register `include_str!` entries.

1. **Create the source skill:** a new directory
   `plugin/skills/<skill-name>/SKILL.md` with `name` and `description`
   frontmatter. Keep the description trigger-first ("Use when …"), under 320
   characters and 45 words; keep the body under 500 lines. Use only allowed
   frontmatter keys (see layer 2 above). A skill directory may additionally
   carry `scripts/`, `references/`, and `assets/` support files — these are
   embedded automatically by the recursive `build.rs` codegen
   (`GENERATED_SKILL_FILES`), so no table edit is needed.
2. **Wire it into the model-invocable index (if model-invocable):** add the
   slug to `hooks::CURSOR_PLUGIN_SKILLS` in `src/hooks/steering.rs`. Workflow
   dispatch that should be explicit-invoke lives as a Cursor native command
   under `plugin/overlays/cursor/commands/<slug>.md`, not as a skill.
3. **Watch the metadata budget:** the summed name+description metadata must
   stay under 6,000 characters. If your addition tips it over, tighten
   descriptions rather than raising the budget.
4. **Run the checks:**

   ```bash
   cargo nextest run -E 'binary(=agent_suite)'
   cargo test --lib covers_the_whole_source_bundle
   ```

   The recursive-embed coverage tests fail if any file under
   `plugin/skills/` is not embedded, and the byte-copy install tests fail if
   the installed tree diverges from the source.

---

## Adding a new ecosystem bundle

To ship a bundle for another agent host (the way `codex-plugin/` mirrors
`cursor-plugin/`):

1. **Create the bundle directory** at the repo root (`<host>-plugin/`) with
   the host's manifest layout and a `README.md` explaining install and any
   host-specific caveats.
2. **Add the integration** in `src/agents/<host>.rs`, embedding bundle files
   with `include_str!` so the installed output is generated from the checked-in
   source, and register it in `src/agents/mod.rs`.
3. **Treat `cursor-plugin/` as the skill source of truth.** Mirror skills
   rather than forking them, and add a parity test in the new integration
   module modeled on `codex_skills_match_the_cursor_source_for_parity`,
   including a divergence allowlist for justified host-specific edits.
4. **Extend the contract tests:** add the host's frontmatter allowlist and a
   contract assertion in
   `tests/agent_suite/plugin_skill_contract_test.rs`, plus a byte-copy
   install-parity test for the generated bundle. Note that
   `tests/agent_suite/` is a single test binary: new modules must be
   registered in `tests/agent_suite/main.rs`.
5. **Vendor the host's schemas** (if it publishes any) under
   `tests/fixtures/<host>-schemas/` and validate the bundle's JSON artifacts
   against them, following the same offline-vendoring rules as the Cursor
   schemas.
6. **Wire it into the sync/CI layers:** add a `Bundle` row (plus any
   divergence exceptions) in `tests/agent_suite/plugin_bundle_sync_test.rs`, and extend
   the CI schema-validation workflow's path filters if the new bundle lives
   outside the existing globs.

---

## Quick reference: what runs where

| Check | Location | Runs in |
|---|---|---|
| Plugin manifest schema + component paths | `tests/agent_suite/plugin_manifest_schema_test.rs` + `tests/fixtures/cursor-schemas/` | `cargo test` (and thereby CI) |
| mcp.json / hooks.json schema validation | `tests/agent_suite/plugin_config_schema_test.rs` | `cargo test` |
| Skill frontmatter contract + size budgets | `tests/agent_suite/plugin_skill_contract_test.rs` | `cargo test` |
| Install byte-copy parity | `tests/agent_suite/plugin_skill_contract_test.rs` | `cargo test` |
| Cursor→Codex skill parity | `src/agents/codex.rs` unit tests | `cargo test` |
| Cross-bundle disk-level sync | `tests/agent_suite/plugin_bundle_sync_test.rs` | `cargo test` |
| Manifest path + rendered output | `tests/agent_suite/agent_test.rs`, `tests/agent_suite/update_plugin_test.rs` | `cargo test` |
| Cursor skill lint rules | `tests/agent_suite/skill_lint_cursor_test.rs` | `cargo test` |
| Claude Code portability rules | `tests/agent_suite/skill_lint_claude_test.rs` | `cargo test` |
| Schema-validation workflow (ajv) | `.github/workflows/plugin-validation.yml` | CI only |
| MCP conformance smoke | `scripts/mcp-conformance-smoke.sh` | manual + CI (`plugin-validation.yml`) |
