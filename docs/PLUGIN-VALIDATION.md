# Plugin and Skill Validation

> **Current layout:** all agent bundles are composed from the shared
> `plugin/` tree. Skills live in `plugin/skills/`; per-host manifests in
> `plugin/.{cursor,codex,claude}-plugin/`; hooks in
> `plugin/hooks/hooks-<host>.json`; host READMEs in `plugin/README-<host>.md`.
> Cursor's workflow dispatchers deploy as native Cursor 1.6+ commands from
> `plugin/overlays/cursor/commands/*.md`; Claude uses `plugin/commands/*.md`.
> `crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs` owns each host's deployed file set.

How the bundled agent plugins (shared `plugin/` tree) and their
skills are validated, where each check runs, and how to extend the system
without breaking the contracts.

This document covers the bundled Cursor, Codex, and Claude integrations:
skills, commands, hooks, rules, manifests, and MCP registrations. It is
unrelated to the language-extractor plugin runtime described in
[`PLUGINS-DESIGN.md`](PLUGINS-DESIGN.md).

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
`tests/fixtures/cursor-schemas/` (Cursor/Codex shapes) and
`tests/fixtures/claude-schemas/` (Claude Code shapes):

| Artifact | Schema | Test |
|---|---|---|
| `plugin/.cursor-plugin/plugin.json` | `cursor-schemas/plugin.schema.json` | `tests/agent_suite/plugin_manifest_schema_test.rs` |
| `plugin/.codex-plugin/plugin.json` | `cursor-schemas/plugin.schema.json` + `interface` extension | `tests/agent_suite/plugin_manifest_schema_test.rs` |
| `plugin/.claude-plugin/plugin.json` | `claude-schemas/plugin.schema.json` | `tests/agent_suite/claude_plugin_schema_test.rs` |
| `plugin/.claude-plugin/marketplace.json` | `claude-schemas/marketplace.schema.json` | `tests/agent_suite/claude_plugin_schema_test.rs` |
| `plugin/.mcp.json` | `cursor-schemas/mcp.schema.json` | `tests/agent_suite/plugin_config_schema_test.rs` |
| `plugin/mcp-cursor.json` (deploys as `mcp.json`) | `cursor-schemas/mcp.schema.json` | `tests/agent_suite/plugin_config_schema_test.rs` |
| `plugin/hooks/hooks-cursor.json` and `plugin/hooks/hooks-codex.json` | `cursor-schemas/hooks.schema.json` | `tests/agent_suite/plugin_config_schema_test.rs` |
| `plugin/hooks/hooks-claude.json` | `claude-schemas/hooks.schema.json` | `tests/agent_suite/claude_plugin_schema_test.rs` |

(The Cursor `marketplace.schema.json` stays vendored for refresh parity; the
Claude marketplace uses its own schema above because Claude entries carry
fields — `category`, `homepage` — that Cursor's marketplace schema rejects.)

The tests use the `jsonschema` crate (dev-dependency only, no network
resolvers — the schemas are self-contained draft-07, and the shipped binary
never validates schemas at runtime).

Beyond schema shape, `tests/agent_suite/plugin_manifest_schema_test.rs` also asserts that
every component path a manifest declares (`skills/`, `hooks/hooks.json`,
`rules/*.mdc`, …) resolves to a real file or directory in the bundle, and
that host manifests share the same plugin `name`. The config-schema tests
include negative cases proving the mcp/hooks schemas actually reject
malformed configs (missing `command`, unknown fields, typo'd event names).

The Cursor plugin/hooks schemas declare `additionalProperties: false`. Codex
marketplaces read an `interface` display-metadata block that Cursor's schema
doesn't define, and the repo-local Codex hook seed carries a top-level
`description` explaining why its `hooks` object is empty. Those two Codex
surfaces are validated against the Cursor schemas plus exactly those
host-specific keys, derived in the tests.

### 2. Skill contract tests (cargo test)

There is one shared `plugin/skills/` tree, so the generic per-file contract is
validated **once** by `tests/agent_suite/shared_skill_contract_test.rs` against
the **intersection contract** — the rules a `SKILL.md` must satisfy to install
cleanly on Claude, Codex, *and* Cursor:

- **Intersection frontmatter whitelist.** Keys ⊆ `{name, description,
  allowed-tools, license, metadata}` (the keys every host's validator accepts).
  `name` matches the directory (kebab-case, ≤64 chars, no reserved
  `claude`/`anthropic` prefix); `name` and `description` are required. A
  Cursor-only key (`disable-model-invocation`/`paths`) would break Codex/Claude,
  so it fails here — that surface belongs in the native commands overlay.
- **Description budget.** 50–320 characters, ≤45 words, trigger-first ("Use
  …"), ends with a period, no angle brackets, unique across the set.
- **Body rules.** Exactly one plain-title H1 (never `# /slug`), no skipped
  heading levels, no body-only `## When to Use` section, ≤500 lines.
- **Hygiene + layout.** No BOM/CRLF/tabs/trailing whitespace, one trailing
  newline, balanced fences, non-empty body; a skill dir holds only `SKILL.md`
  plus the supported resource dirs (`agents/`, `scripts/`, `references/`,
  `assets/`) and no auxiliary docs (`README.md`, `CHANGELOG.md`, …).

The same test validates the host-extra surfaces **separately**: the Cursor
native commands (`plugin/overlays/cursor/commands/*.md`, each a `# /slug` H1
matching its file name) and the Cursor agent overlay.

`tests/agent_suite/plugin_skill_contract_test.rs` now owns only what is *not*
in the intersection: the aggregate 6,000-char metadata budget, the optional
`agents/openai.yaml` marketplace contract, Codex `quick_validate.py`, and
**byte-copy install parity** (installing the Cursor or Codex integration into a
temp home must produce a byte-identical copy of the source skill tree — catches
install-time mutation and missing embeds; see
[Adding a skill](#adding-a-skill-correctly)).

`tests/agent_suite/skill_lint_cursor_test.rs` keeps only the Cursor-specific
reference-integrity rules (skill/tool/link resolution, `paths` glob scoping) and
the native-command lint, adapted from community SKILL.md linters (skillmark,
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

### 3. Shared source and host projections (cargo test)

There is one source tree: `plugin/`. Host bundles are filtered projections of
that tree, composed by `crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs`:

- Claude deploys manifest, MCP config, hooks, agents, commands, README, and
  every file under `plugin/skills/`.
- Codex deploys manifest, MCP config, hooks, README, and every file under
  `plugin/skills/`.
- Cursor deploys its manifest, MCP config, hooks, rules, README, native
  commands, Cursor agents, and the shared skill files **except** the
  `tracedecay-*` dispatcher skills. Those slugs are native Cursor commands.

`crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs` unit tests check that recursive skill embedding
matches every file under `plugin/skills/`, that Cursor filtering stays
intentional, and that the host file lists remain deterministic. Contract tests
then validate each projected surface: shared skills once, Cursor commands and
agents separately, and Claude/Codex host metadata separately.

### 4. Rendered-output and manifest-path validation (cargo test)

Beyond validating the *source* bundles, install-time output is validated.

**Manifest location.** Cursor requires the manifest at
`.cursor-plugin/plugin.json` inside the plugin root (per
[cursor.com/docs/plugins](https://cursor.com/docs/plugins) and the official
[cursor/plugins](https://github.com/cursor/plugins) marketplace repo). This
repo already conforms — `plugin/.cursor-plugin/plugin.json` in source,
and `crates/tracedecay-agent-hosts/src/agents/cursor.rs` renders it to
`~/.cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json`. The layout is
pinned by existing assertions in `tests/agent_suite/agent_cursor_test.rs` and
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

`tests/agent_suite/skill_lint_claude_test.rs` lints the shared skills against
Claude Code / Agent Skills portability rules. The rules (sources cited in the
test's module docs) include: frontmatter keys limited to Claude-Code-documented
fields, kebab-case `name` matching the directory
(≤ 64 chars, no XML tags, no reserved words `anthropic`/`claude`),
`description` non-empty with no angle brackets (≤ 1,024 chars, and
`description` + `when_to_use` ≤ 1,536 chars — Claude Code truncates listings
beyond that), and the shared 6,000-char per-bundle metadata budget.

Cursor workflow dispatchers are native commands now, so shared skills do not
need Cursor-only `disable-model-invocation` frontmatter.

### 6. Checks outside the Rust test harness

Most validation deliberately lives in `cargo test`, because the existing
`ci.yml` `test` job is configured to run the full suite on every PR — a check
that can be a `#[test]` needs no new YAML. That describes what CI is wired to
invoke, not a green baseline: the aggregate suite has not completed
successfully on this branch, so a new `#[test]` joins a suite whose overall
state is unverified rather than a known-passing one. CI-only additions are
limited to what cargo can't do:

- **Schema-validation workflow**
  (`.github/workflows/plugin-validation.yml`, `manifest-schema` job). Mirrors
  the official `cursor/plugins` marketplace validation: `ajv` compiles all
  four vendored schemas (so a broken schema edit fails even when no manifest
  changed), validates the Cursor manifest against `plugin.schema.json`, and
  parse-checks every `*.json` in `plugin/`. The Codex manifest is only
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

To ship a bundle for another agent host:

1. **Add host-specific source files** under `plugin/` or
   `plugin/overlays/<host>/`, keeping shared skills in `plugin/skills/`.
2. **Add the integration** in `crates/tracedecay-agent-hosts/src/agents/<host>.rs`, compose its deployed
   file set from `crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs`, and register it in
   `crates/tracedecay-agent-hosts/src/agents/mod.rs`.
3. **Extend the contract tests:** add any host frontmatter allowlist,
   host-specific manifest/config assertions, and install-output validation.
   `tests/agent_suite/` is a single test binary, so new modules must be
   registered in `tests/agent_suite/main.rs`.
4. **Keep skills shared.** Host-specific dispatch belongs in commands, rules,
   hooks, or overlays, not forked copies of `plugin/skills/*/SKILL.md`.
5. **Vendor the host's schemas** (if it publishes any) under
   `tests/fixtures/<host>-schemas/` and validate the bundle's JSON artifacts
   against them, following the same offline-vendoring rules as the Cursor
   schemas.
6. **Wire it into CI:** extend `.github/workflows/plugin-validation.yml` path
   filters if the new host adds files outside the existing globs.

---

## Quick reference: what runs where

| Check | Location | Runs in |
|---|---|---|
| Plugin manifest schema + component paths | `tests/agent_suite/plugin_manifest_schema_test.rs` + `tests/fixtures/cursor-schemas/` | `cargo test` (and thereby CI) |
| mcp.json / hooks.json schema validation | `tests/agent_suite/plugin_config_schema_test.rs` | `cargo test` |
| Unified intersection skill contract (frontmatter/description/body/hygiene/layout) + Cursor commands + agent overlay | `tests/agent_suite/shared_skill_contract_test.rs` | `cargo test` |
| Metadata budget + openai.yaml + per-host frontmatter + install byte-copy parity | `tests/agent_suite/plugin_skill_contract_test.rs` | `cargo test` |
| Recursive-embed coverage (skill tree fully embedded) | `crates/tracedecay-agent-hosts/src/agents/{claude,codex,cursor,plugin_bundle}.rs` unit tests | `cargo test` |
| Manifest path + rendered output | `tests/agent_suite/agent_cursor_test.rs`, `tests/agent_suite/update_plugin_test.rs` | `cargo test` |
| Cursor reference-integrity + native-command lint | `tests/agent_suite/skill_lint_cursor_test.rs` | `cargo test` |
| Claude Code portability rules | `tests/agent_suite/skill_lint_claude_test.rs` | `cargo test` |
| Schema-validation workflow (ajv) | `.github/workflows/plugin-validation.yml` | CI only |
| MCP conformance smoke | `scripts/mcp-conformance-smoke.sh` | manual + CI (`plugin-validation.yml`) |
