# `tracedecay tool` CLI arguments, reimagined for the AI-agent consumer

> **Archived record — not implementation authority.** This document preserves
> historical intent and evidence. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy. Exact tests and counts, source-string
> checks, branch/commit/worktree choreography, snapshots, receipts,
> attestations, PR packets, and gate matrices below are not rebuild
> instructions; validate current parser, runtime, and product behavior directly.

Date: 2026-07-04
Branch: `codex/cli-args-stdin` (PR #286, "Support stdin for tool args")
Status: implemented on `codex/cli-args-stdin` — the branch now includes the
JSON-first CLI contract, validation gate, `--dry-run`, corrective errors,
help/skill/steering updates, per-key repairs, and hermetic eval coverage.

---

## 1. Context and scope

`tracedecay tool <name> [args...]` invokes any of the ~100 MCP tools from the
shell. Its consumers, in rough order of real traffic:

1. **AI coding agents** shelling out because MCP is unavailable: the server
   errored/timed out/disconnected, the host never configured it, or the
   context (subagent, hook, CI script) has shell access but no MCP client.
   Every plugin skill, steering hook, and prompt rule routes agents here
   (`src/hooks/steering.rs:137-143`, `plugin/skills/using-the-cli/SKILL.md`,
   `plugin/rules/tracedecay.mdc:35`, `src/agents/mod.rs:816`,
   `src/agents/hermes/templates/skill.md:15`).
2. **Generated machine glue** — the Hermes plugin's `tools.py`
   (`src/agents/hermes/templates.rs:233-272`) dispatches every tool call
   through this CLI.
3. **Humans** debugging, exploring, or scripting ad hoc.

PR #286 added stdin support to the `--args` whole-payload escape hatch
(`--args -`, `--args @-`, bare file path, back-compat `@file`), plus per-key
`@path`/`@-` file/stdin values with memoized stdin, and a two-scenario
hermetic eval corpus. That work is committed on this branch
(`src/tool_command.rs:1-30` module doc; `f4af7ce4`, `846081c8`, `14ae43de`).

This plan steps back and asks the prior question the PR did not: **what
should the agent-facing argument surface be at all?** It evaluates the
abstraction first (Section 2), documents the current mechanics precisely
(Section 3), ranks the observed agent-facing frictions with empirical
evidence from the dev binary (Section 4), proposes the design (Section 5),
specifies the discoverability changes so the taught model matches the parser
(Section 6), defines a detailed eval plan to prove the change with real
Sonnet and Codex sessions (Section 7), and phases the rollout (Section 8).

Everything below cites the state of this branch at commit `14ae43de`. All
empirical outputs come from this worktree's debug binary run against an
isolated `TRACEDECAY_DATA_DIR` and a throwaway indexed project (Appendix A).

---

## 2. The abstraction question: should agents get a `--key value` surface at all?

Before polishing the argument parser, decide what the agent-facing contract
is. Four candidate models, evaluated against the code as it exists.

### 2.0 The 30-second version of how it works today

`tracedecay tool` is declared in clap as a name plus a raw trailing vector
(`src/cli.rs:101-114`, `trailing_var_arg = true, allow_hyphen_values =
true`) — clap does no parsing of tool args. A hand-rolled parser
(`src/tool_command.rs:351-483`) walks the tool's JSON Schema
(`src/mcp/tools/definitions.rs`) to convert `--key value` strings into a
`serde_json::Value` object, with per-type coercion (`coerce_value`,
`src/tool_command.rs:514-560`). The finished object is then handed —
verbatim — to the daemon over a Unix socket as a standard MCP
`tools/call` JSON-RPC request (`dispatch_daemon_tool` →
`call_default_tool` → `call_tool`, `src/daemon.rs:1177-1244`), falling
back to an in-process dispatch of the same registry when the daemon is
down (`src/tool_command.rs:212-267`).

So the CLI is *already* a thin MCP client. The only thing the arg surface
does is **construct the `params.arguments` JSON object from argv**. That
framing is what makes the abstraction question sharp: the per-key grammar
is a lossy re-encoding of a JSON object into shell words, immediately
decoded back into JSON.

### 2.1 Option 1 — status quo: `--key value` with schema coercion as the agent path

The taught model everywhere today ("every tool is also a shell command:
`tracedecay tool <name> --key value`"). For an agent this means learning a
second parameter language on top of the MCP schema it already knows:

- booleans need explicit values (`--include-code true`; bare `--include-code`
  eats the next token — Appendix A.3/A.4);
- `--key=value` is not accepted, unlike every clap subcommand in the same
  binary (Appendix A.1/A.2);
- arrays are repetition or comma-splitting (`finalize_arrays`,
  `src/tool_command.rs:587-617`) — which **destroys** inline JSON for
  array-of-array/object params (Appendix A.7);
- nested objects and enum constraints are not expressible/validated at all
  (`coerce_value` falls through to string, enums pass unchecked —
  Appendix A.8);
- unknown keys are silently forwarded and silently ignored by handlers
  (Appendix A.6) — a typo produces wrong behavior with no error;
- any value containing quotes/newlines is a shell-quoting minefield in the
  one-shot command an agent must emit.

Fixing all of that is possible (Section 5.4 keeps a subset), but the end
state is still: an agent that knows `{"path": ..., "replacements":
[[...]]}` must transliterate it into a different grammar, correctly, in one
shot, under shell quoting. Every rule added to make that grammar richer is
one more rule the agent must have internalized *before* the failure moment
in which it reaches for the CLI. **Rejected as the agent-primary path.**

### 2.2 Option 2 — JSON-first agent path: `--key value` is human sugar; agents pass the MCP arguments object

Treat the argument surface as two contracts:

- **Agent contract**: `tracedecay tool <name> --args <payload>` where the
  payload **is exactly the MCP `arguments` object** the agent would have
  sent over MCP — inline for short quote-free payloads, stdin (`--args -`,
  typically a quoted heredoc) or a file for everything else. Nothing new to
  learn beyond one sentence, because the agent already knows every tool's
  schema; MCP knowledge transfers byte-for-byte.
- **Human contract**: `--key value` (plus positional-required-string
  binding) stays for interactive use, where flags genuinely beat typing
  JSON.

Evidence this is the natural machine interface, from this repo itself:

- The Hermes plugin's generated `tools.py` — the one *programmatic* consumer
  of `tracedecay tool` in the codebase — never uses `--key value`. It
  serializes the arguments dict and passes `--json --args <payload>`,
  spilling to `--args @tempfile` above ~100 KB to dodge Linux's 128 KiB
  per-argv-string cap (`src/agents/hermes/templates.rs:89-98,260-272`;
  pinned by `tests/agent_suite/agent_test.rs:1117-1120`).
- `memory curate --llm-ops <file|->` established the same whole-payload
  convention for LLM-constructed JSON (`src/commands.rs:46-66`).
- PR #286 already built the payload plumbing: inline / `-` / bare path /
  `@file` / `@-`, memoized stdin (`resolve_args_payload`,
  `src/tool_command.rs:491-509`).

Shell quoting — the classic objection to JSON-on-the-CLI — is solved by the
stdin form with a quoted heredoc, a pattern agents already use daily for
`git commit -m "$(cat <<'EOF' ...)"`:

```bash
tracedecay tool multi_str_replace --args - <<'JSON'
{"path":"src/lib.rs","replacements":[["old, with 'quotes'","new $body"]]}
JSON
```

No escaping, no argv cap, arbitrary newlines, byte-exact MCP parity.
**Recommended.** The rest of this plan is the working-out of this option.

### 2.3 Option 3 — raw daemon/MCP passthrough: the CLI accepts a JSON-RPC frame

The maximal-parity idea: `tracedecay tool` (or a new `tracedecay rpc`)
reads a whole `tools/call` JSON-RPC request from stdin, forwards it to the
daemon socket, prints the response. Research findings:

- The daemon already speaks exactly this protocol: a handshake line then
  line-delimited JSON-RPC, with `tools/call` handled server-side
  (`src/daemon.rs:2145-2184`); `tracedecay serve` proxies full MCP stdio to
  it (`proxy_stdio_to_default_daemon`, `src/daemon.rs:1168-1174`;
  `src/cli.rs:289-291`).
- But the CLI *already is* this passthrough for the part that carries
  information: `call_tool` builds the envelope — `jsonrpc`, `id`, `method:
  "tools/call"`, `params.name` — from values the CLI already has
  (`src/daemon.rs:1183-1193`). The only agent-authored content in the frame
  is `params.arguments`, which is precisely what `--args` transports.
- A raw-frame mode would *add* agent-visible failure modes (wrong method
  string, malformed envelope, id bookkeeping) while *removing* things the
  CLI quietly does correctly: the handshake carries client identity,
  profile root, global-DB path, and project routing
  (`DaemonHandshake::for_current_client`, `src/daemon.rs:195-231`;
  project resolution in `DaemonToolDispatch::project_scoped`,
  `src/tool_command.rs:181-194`), and `dispatch_daemon_tool` falls back to
  in-process execution when the daemon socket is absent
  (`src/tool_command.rs:269-296`) — on Windows there is no socket at all
  (`src/daemon.rs` `#[cfg(not(unix))]` stubs). An agent hand-writing frames
  would have to replicate an undocumented, version-coupled handshake or
  lose all of that.

Option 3 therefore collapses into Option 2: **passthrough of the
`arguments` object, not the envelope**. The envelope is boilerplate the CLI
should keep owning. Rejected as a separate surface; one nicety survives as
`--dry-run` (Section 5.3), which prints the fully-resolved arguments object
(and optionally the frame) without dispatching — giving scripts and evals
the "show me the request" affordance without a second protocol.

### 2.4 Option 4 — does the agent need this surface at all?

When does an agent actually land here?

- **MCP transport failure mid-session** — the steering text injected into
  every Codex session names this exact moment and prescribes the CLI
  (`src/hooks/steering.rs:137-143`); the `using-the-cli` skill is the
  Claude-side equivalent (its trigger description names failures,
  timeouts, disconnected/unconfigured servers).
- **Contexts that never had MCP** — subagents and hooks with shell but no
  MCP client (`plugin/skills/using-the-cli/SKILL.md:50-52`), CI scripts,
  and hosts where tracedecay's MCP server isn't registered. These are
  by-design consumers, not failure recovery.
- **Permission-denied MCP** in restricted harness configurations where
  `Bash` is allowed but the MCP tool isn't.

Alternative remedies considered:

- *Re-establish MCP* (restart server, ToolSearch reload): host-level and
  frequently outside the agent's control; the skill already covers the
  "deferred but healthy" case separately (`SKILL.md:63-69`). Not a
  substitute for the genuinely-no-MCP contexts.
- *`tracedecay serve` as ad hoc MCP*: an agent could spawn it and speak MCP
  over stdio, but a one-shot shell tool cannot reasonably hold a
  bidirectional initialize/call/shutdown conversation. Impractical.
- *Only expose plumbing (`--print-request`) and let the agent pipe frames
  itself*: strictly worse than Option 3 for the same reasons.

Conclusion: the fallback surface must exist, agents are its primary
consumers, and the **only part of it agents should have to think about is
the arguments object they already know**. The per-key grammar continues to
exist for humans — but every agent-facing document, hook, and error message
should converge on the JSON path.

### 2.5 Tensions resolved explicitly

| Tension | Resolution |
|---|---|
| Human vs agent ergonomics | Two documented contracts on one command: `--key value` for humans, `--args` JSON for agents/scripts. Neither is deprecated; they are *taught to different audiences*. |
| MCP parity vs CLI convention | Parity wins for agents: the payload is the MCP `arguments` object, so nothing new to learn. The CLI-only conventions (`--json`, `--project`, `--dry-run`) are transport concerns, not argument concerns. |
| Forgiving vs unambiguous parsing | Per-key parsing stays forgiving for humans but gains a single validation gate (Section 5.2) that turns every silent divergence into a corrective error. JSON path is unambiguous by construction. |
| Back-compat vs one-clean-rule | All currently-working invocations keep working (Section 8). The one intentional break: unknown keys and invalid enum values stop being silently ignored — that silence is the bug. |
| Altitude: schema coercion in the CLI duplicates the MCP layer | Correct diagnosis: `coerce_value` re-derives types the schema already declares, and handlers re-validate (or fail to). The fix is not more coercion but one schema-driven validation pass over the *final JSON object*, shared by both paths, next to the schemas it validates. |

---

## 3. Current state, precisely

### 3.1 Parse pipeline

`run()` (`src/tool_command.rs:81-132`): resolve tool name via
`canonical_tool_name` (strip `tracedecay_`, dash→underscore, alias
`query`→`search`; `src/tool_command.rs:49,148-156`) → `parse_invocation` →
help or dispatch.

`parse_invocation_with_stdin` (`src/tool_command.rs:351-483`), one pass over
the raw arg vector:

- Reserved flags: `-h/--help` short-circuits; `--json` sets raw output;
  `--project` takes a value (`:387-395`).
- `--args <v>` (`:396-412`): `resolve_args_payload`
  (`:491-509`) resolves inline JSON (leading `{`/`[`) verbatim, `-` →
  memoized stdin (`:330-349`), `@file`/`@-` via `resolve_at_file`, anything
  else as a bare file path. Must parse to a JSON **object**; mutually
  exclusive with any other tool flag or positional (`:426-435`).
- Any other `--flag` (`:414-421`): key = kebab→snake, next token is the
  value (`take_value`, `:620-624`), `@`-prefixed values are read from
  file/stdin (`resolve_at_file`, `:630-642`), then `coerce_value`
  (`:514-560`) coerces by schema type: string pass-through; boolean accepts
  `true/1/yes/on`/`false/0/no/off` else errors; integer/number parse with
  whole-number-stays-integer care; **`array` → returns the raw string**;
  **anything else (incl. `object`) → returns the raw string**. Repeated
  flags accumulate into an array (`merge_value`, `:569-581`).
- Non-flag tokens are positionals, bound in-order to *required* properties
  not already set (`:439-466`); leftovers error.
- Missing required params error (`:468-478`).
- `finalize_arrays` (`:480,587-617`): for every schema property of type
  `array` whose collected value is a single string, **split on commas** if
  the string contains any, else wrap as a one-element array. No JSON
  detection, no `items`-type awareness.

Notable absences: no `--key=value` handling (the whole token becomes an
unknown key), no unknown-key rejection (missing `prop_schema` just means
"coerce as string and forward"), no enum validation anywhere, no
object-typed value parsing.

### 3.2 Dispatch

The parsed object goes to the daemon as MCP `tools/call`
(`src/daemon.rs:1177-1244`), or in-process through the same registry when
the socket is unavailable (`src/tool_command.rs:227-296`). Neither the
daemon (`src/daemon.rs:2181-2184` checks only that `params.name` exists)
nor the handlers validate arguments against the schema; handlers `.get()`
fields and default what's missing. Output: joined `content[*].text` blocks,
or raw JSON with `--json` (`:298-326`).

### 3.3 Schema surface the parser must cover

From `src/mcp/tools/definitions.rs` (100 tools listed by the binary):

- ~23 array-typed params; most are `array<string>` (comma-split works),
  but at least four are arrays of arrays/objects: `multi_str_replace.
  replacements` (`:1726-1748`, `[[old,new],…]`), and message/query arrays
  on `lcm_expand_query` (`:2955`), `lcm_preflight` (`:3009`),
  `lcm_compress` (`:3100`).
- 30 `enum` declarations (e.g. `gini.metric`
  `["complexity","lines","fan_in","fan_out","members"]`, `:1792-1811`).
- Nested-object params: `project_selector` on `search`/`context`/etc.
  (`project_selector_object`, `:946-965`).
- Long multi-line strings: `str_replace.old_str/new_str`,
  `replace_symbol.new_source`, `insert_at.content`, `ast_grep_rewrite.
  pattern/rewrite` (`:3231-3259`), `diagnose.cargo_output`,
  `fact_store` text.

### 3.4 What the agent is taught, and where it diverges from the parser

| Surface | What it says | Divergence |
|---|---|---|
| Codex steering, injected every session (`src/hooks/steering.rs:137-143`) | "every tool is also a shell command: `tracedecay tool <name> --key value`" | Teaches the grammar with the most traps; never mentions `--args`/stdin. |
| `using-the-cli` skill (`plugin/skills/using-the-cli/SKILL.md:18-23`) | `--key value` first; `--args`/`@`/stdin as a parenthetical | JSON path presented as an afterthought, not the machine path. |
| Arg catalog (`plugin/skills/using-the-cli/references/tool-arg-catalog.md:56`) | `multi_str_replace` required flags: `--path`, `--replacements` (`[[old,new],…]`) | **Actively teaches a shape the parser destroys** — comma-splitting mangles the JSON (Appendix A.7). Also stale (`body` documented as `--node-id (or --symbol)`; schema is `symbol` + `limit`, `definitions.rs:3290-3312`). |
| Per-tool `--help` (`render_tool_cli_help`, `src/mcp/tools/mod.rs:85-160`) | `--replacements  array  required  "Array of [old_str, new_str] pairs"` | Invites inline per-key JSON that will be mangled; **does not print enum values** (Appendix A.8 help output); does not say array/object params need `--args`. |
| `tracedecay tool --help` trailer (`src/cli/help.rs:86-106`) | Correctly says `--args @file.json … required for array/object parameters` | The one place that states the rule — but it's on the *subcommand* help, which the discovery flow (list → per-tool `--help`) skips right past. |
| Bare list footer (`src/tool_command.rs:693-697`) vs per-tool footer (`mod.rs:150-157`) vs subcommand trailer | Three different reserved-flag/stdin footnotes | Wording drift; per-tool footer still leads with the `@` sigil model, list footer with the new whole-payload model. |
| ~10 other skills + Cursor rule + Hermes template + install prompt rules (`plugin/skills/*/SKILL.md`, `plugin/rules/tracedecay.mdc:35`, `src/agents/hermes/templates/skill.md:15`, `src/agents/mod.rs:816`) | All repeat `--key value` verbatim | The grammar is ossified in a dozen prose surfaces — and beyond the repo, into users' own CLAUDE.md files. Whatever we teach next should be *stable*, which favors the schema-parity JSON contract over flag ergonomics. |

The net mental model handed to an agent — "alternating `--key value`
flags, kebab-case" — is **correct for scalar-only tools and wrong for
exactly the tools whose payloads are hardest to construct**, with the
authoritative reference (the catalog) actively wrong for
`multi_str_replace`.

---

## 4. Usability analysis for an AI-agent consumer

An LLM invoking a CLI constructs the entire command in one shot from
steering + `--help` + prior knowledge; it cannot tab-complete or
experiment cheaply; it generalizes one convention across all 100 tools; and
its recovery loop is exactly as good as the error text. Ranked friction
points, each grounded in an empirical run (Appendix A) or code:

**F1 — Silent wrong behavior (worst class: no error to learn from).**
- Typo'd/unknown optional flag: `tool search --query gamma --limt 2` runs
  successfully with the default limit; `--limt` is forwarded and ignored
  (A.6; `src/tool_command.rs:414-421` — no schema check).
- Invalid enum: `tool gini --metric bogus` returns a computed result
  labelled `metric: bogus` (A.8) — 30 enum params, zero validation.
- Object-typed param per-key: `--project-selector '{"project_id":"x"}'`
  arrives as a *string*; handlers ignore it (`coerce_value` fall-through,
  `:558`).

**F2 — The taught shape for array-of-JSON params fails, with a
non-corrective error.** `tool multi_str_replace --path lib.rs
--replacements '[["alpha","gamma"]]'` — the exact catalog shape — dies with
`each replacement must be an array of exactly 2 strings` (A.7), a
*handler* error produced after comma-splitting mangled the JSON, hinting
nothing about `--args`. The one-shot construction fails and the retry has
no signpost.

**F3 — GNU `=` form rejected, confusingly.** `--query=foo` →
``flag `--query=foo` requires a value`` (A.1); worse, `--query=foo --json`
consumes `--json` as the unknown key's value and then reports `missing
required parameter --query` (A.2). Clap accepts `=` everywhere else in the
binary, so an agent's prior from `tracedecay sync --path=X` actively
misleads it.

**F4 — Boolean flags aren't presence flags.** `--include-code` alone at
end: ``requires a value``; mid-command it swallows the next token —
`--include-code --json` at least errors with `expected a boolean
(true/false), got '--json'` (A.3/A.4), which is corrective, but the
`requires a value` variant never states the fix (`pass true or false`).

**F5 — Shell quoting of inline JSON.** Any `--args '{...}'` or per-key
value containing a single quote forces the `'"'"'` dance; multi-line
bodies (replacement text, ast-grep patterns, cargo output) are effectively
impossible inline. The escape (`@file`, `@-`, `--args -`) exists and is
good — but it's taught as a footnote (Section 3.4) rather than as *the*
agent form, and nothing in an error message ever points to it.

**F6 — `--help` is insufficient for one-shot construction on the hard
tools.** No enum values (A.8 help), no `items` shape for arrays, no
example, no statement that array/object params require `--args`. For
`multi_str_replace`, help + catalog steer the agent straight into F2.

**F7 — Single-dash and positional misbinding.** `-query foo` silently
binds `-query` to the required `query` and errors about leftover `foo`
(A.5) — the message points at the wrong token. `allow_hyphen_values`
(`src/cli.rs:112`) means clap can't catch it.

**F8 — Minor consistency debt.** `--json` (raw envelope) vs per-tool
`--format json` (markdown/JSON payload switch) is a two-knob surprise;
three drifting footers (Section 3.4); usage errors print as
`Error: config error: …` (`TraceDecayError::Config` display), mislabeling
user-input problems as configuration problems.

What already works well and must be preserved: required-param enforcement
with good message shape (``missing required parameter `--query` for tool
`search` ``), name normalization (prefix/dash/alias,
`:148-156`), repetition-for-arrays, `@file`/`@-` per-key, the whole-payload
`--args` family with stdin memoization, positional binding for quick human
queries, and grouped discovery via bare `tracedecay tool` (`:647-698`).

---

## 5. Reimagined design

### 5.1 The one generalizable rule

> **The arguments of `tracedecay tool <name>` are the tool's MCP
> `arguments` object.** Pass it whole with `--args` (inline JSON, `-` for
> stdin — use a quoted heredoc, or a file path). Or, for quick scalar
> calls, spell top-level fields as `--key value` flags; values are
> interpreted by the tool's schema, and anything that isn't a scalar is
> JSON.

Everything an agent needs beyond its existing MCP knowledge is that one
paragraph. How each argument kind maps:

| Kind | Agent form (taught) | Human form (kept) |
|---|---|---|
| Whole payload | `--args -` + `<<'JSON'` heredoc; `--args '{…}'` inline when short and quote-free; `--args payload.json` | same |
| Scalar string / integer / number | inside `--args` | `--key value`, `--key=value` (new), positional for required strings |
| Boolean | inside `--args` | `--key true|false` (unchanged; corrective error gains the exact fix text) |
| Enum | inside `--args` | `--key value`, now validated with allowed values in the error |
| Array of strings | inside `--args` | repeat `--key a --key b`, or `--key a,b`, or `--key '["a","b"]'` (new: JSON accepted) |
| Array of arrays/objects | inside `--args` (the only sane form) | `--key '<json>'` (new: parsed as JSON because schema type is array; comma-split only applies when the value doesn't parse as JSON) |
| Nested object | inside `--args` | `--key '{"…":…}'` (new: parsed as JSON because schema type is object) |
| Multi-line string | inside the heredoc payload | `--key @file` / `--key @-` (unchanged) |
| Payload > 128 KiB argv cap | `--args -` or `--args file` | same |

### 5.2 One validation gate, shared by both paths

New function in `src/tool_command.rs` (name suggestion:
`validate_tool_args(def: &ToolDefinition, args: &Map<String, Value>) ->
Result<()>`), called at the end of `parse_invocation_with_stdin` on the
final object — whether it came from `--args` or from per-key collection
(insert after `finalize_arrays`, `:480`, and on the `--args` branch,
`:426-435`). It walks `input_schema` once and enforces:

1. **Unknown keys** → error listing the unknown key, a did-you-mean
   suggestion (nearest by edit distance over property names), and the
   valid keys. Catches F1-typos on *both* paths (an `--args` payload with
   a misspelled key gets the same protection MCP hosts give).
2. **Enum membership** → error with the allowed values verbatim (F1-enums).
3. **Type agreement** on the final JSON (string vs array vs object vs
   number/boolean) → corrective error naming the expected JSON type and
   showing the `--args -` heredoc form for non-scalars (F2 backstop).
4. **Required presence** → keep the existing message (`:468-478`), now also
   enforced for `--args` payloads (today a payload missing required keys
   goes to the handler and fails handler-side or silently).

Implementation notes: hand-roll the walker (~100 lines; the schemas use
only `type`/`enum`/`items`/`required`/`properties`) rather than adding a
`jsonschema` crate dependency; validate against the same
`get_tool_definitions()` the dispatch uses so conditionally-advertised
tools (`ast_grep_rewrite` retention, `definitions.rs:340`) stay
consistent; treat schemas without `properties` as opaque (skip validation)
so profile-scoped/dynamic tools cannot be bricked by a stale walker.

This is the altitude fix: validation happens once, on the final JSON,
next to the schema — not scattered through string coercion, and not
duplicated per-handler. It also makes the CLI *stricter than the daemon*,
which is correct: the daemon trusts validated MCP clients
(`src/daemon.rs:2181-2184`); the CLI's caller is the thing that needs the
teaching.

### 5.3 `--dry-run` (reserved flag)

Parse + validate + print the final arguments object as pretty JSON to
stdout, exit 0 (or the corrective error, exit ≠ 0) — no daemon, no
handler, no side effects. One `if` in `run()` after `parse_invocation`
(`src/tool_command.rs:102-113`), one field in `ParsedInvocation`
(`:137-142`). Value: agents can self-check destructive edit-tool payloads
before applying; evals get a deterministic, side-effect-free probe of
"did the agent construct the right object" (Section 7.4); humans get
"show me what would be sent". This subsumes the `--print-request` idea
from Option 3 — if desired later, `--dry-run --json` can print the full
`tools/call` frame, but the arguments object is the useful part.

### 5.4 Per-key repairs (human path, kept deliberately small)

In `parse_invocation_with_stdin`:

1. **`--key=value`** (F3): in the `flag if flag.starts_with("--")` arm
   (`:414`), split on the first `=` before kebab→snake conversion; the
   remainder is the value (no `take_value`). `--args=…`, `--project=…`
   likewise in the reserved-flag matches.
2. **JSON-typed per-key values** (F2, F1-objects): in `coerce_value`
   (`:514-560`), for schema type `array` or `object`, first attempt
   `serde_json::from_str`; accept if the parsed type matches the schema
   type; otherwise fall back to current behavior (string, later
   comma-split for arrays) so `--keywords auth,login` keeps working.
   `finalize_arrays` (`:587-617`) then skips values that are already
   arrays (it does today, `:612`).
3. **Corrective boolean/missing-value errors** (F4): `take_value` error
   becomes ``flag `--include-code` requires a value — pass `--include-code
   true` or `--include-code false``` for booleans (thread the schema type
   through, or special-case in the caller); generic flags get ``flag `--x`
   requires a value — write `--x <value>` or `--x=<value>` ``.
4. **Single-dash guard** (F7): a positional starting with a single `-` and
   matching a known property name (after dash→underscore) errors with
   ``did you mean `--query`?`` instead of binding as a positional.

Explicitly *not* doing: presence-style booleans (`--include-code` alone).
With `allow_hyphen_values` and positionals in play, presence booleans are
ambiguous (`--before src/x.rs` on `insert_at` — flag-then-positional or
flag-with-value?); the corrective error is the safe fix.

### 5.5 The corrective-error contract

Every rejection must tell the agent exactly how to fix the call — the
error message is the CLI's tab-completion. The contract to implement and
test (messages abbreviated; all end by pointing at `--help` only when the
fix isn't already fully stated):

| Rejection | Today (`src/tool_command.rs`) | Contract |
|---|---|---|
| Unknown tool (`:94-99`) | names the tool, points at list | + nearest-name suggestion (`did you mean 'dead_code'?`) |
| Unknown key (new) | *silent* (F1) | ``unknown parameter `--limt` for `search` — did you mean `--limit`? Valid: --query (required), --limit, --format, --project-id, --project-path, --project-selector`` |
| Invalid enum (new) | *silent* (F1) | ``--metric: `bogus` is not one of: complexity, lines, fan_in, fan_out, members`` |
| Array/object param given a non-JSON scalar (new) | comma-split mangle → handler error (F2) | ``--replacements expects a JSON array. Pass JSON: --replacements '[["old","new"]]' — or the whole payload via stdin: tracedecay tool multi_str_replace --args - <<'JSON' … JSON`` |
| `--key=value` (F3) | ``flag `--query=foo` requires a value`` | *accepted* (5.4.1); until then: ``write `--query foo` or `--query=foo` `` |
| Bare boolean (F4) | ``requires a value`` | ``--include-code requires true or false, e.g. `--include-code true` `` |
| Boolean swallowed a flag (`:522-531`) | states expected/got (good) | keep; append the `true/false` example |
| Missing flag value (`:620-624`) | ``flag `--x` requires a value`` | + `--x <value>` example (5.4.3) |
| Missing required (`:468-478`) | good | keep; append one-line usage: ``e.g. tracedecay tool search --query "<text>"`` |
| `--args` invalid JSON (`:403-406`) | serde error, positioned | + ``if the payload contains quotes or newlines, pipe it: --args - <<'JSON' … JSON`` |
| `--args` non-object (`:407-411`) | ``must be a JSON object`` | + ``the same object you would pass as MCP arguments, e.g. {"query":"…"}`` |
| `--args` + other flags (`:426-435`) | states exclusivity | + ``either put everything in --args, or use only --key value flags`` |
| `--args` unreadable path (`:503-508`) | states the three forms (good) | keep |
| `@file` missing (`:636-638`) | ``failed to read @path: <io>`` | + note the path is cwd-relative; suggest `--args -` for literals that begin with `@` |
| Unexpected positional (`:456-465`) | suggests flags + help (good) | + if it starts with `-`, the 5.4.4 did-you-mean |
| stdin read failure (`:342-345`) | io error | keep |

Cosmetic but worthwhile: introduce a distinct display prefix for these
(`usage error:` rather than `config error:`) — either a new
`TraceDecayError` variant or message-prefix convention (F8).

### 5.6 Alternative considered and rejected: full clap-native dynamic subcommands

Generate a real clap `Command` per tool at startup (schema → typed
`Arg`s), getting `=` handling, unknown-flag errors, and `--help` for free.
Rejected: clap's dynamic builder would re-encode JSON Schema into clap's
type system (losing enums-with-values-in-errors unless hand-fed,
struggling with array-of-array items), boot-time cost on a "hot-ish path"
that deliberately skips even the reinstall scan (`src/main.rs:790-812`),
and it polishes exactly the surface Section 2 demoted — while `--args`
passthrough, positionals, and `@` values would still need the hand-rolled
layer. The 100-line validation walker buys the same agent-visible wins at
a fraction of the risk, and keeps one parser instead of two.

### 5.7 Files and functions touched (implementer map)

| Change | Where |
|---|---|
| `validate_tool_args` + call sites (both paths) | `src/tool_command.rs` (new fn; hook at `:426-435` and after `:480`) |
| `--dry-run` flag + `ParsedInvocation.dry_run` + early return | `src/tool_command.rs:81-142,387-395` |
| `--key=value` split; boolean/missing-value error text; single-dash guard | `src/tool_command.rs:387-424,514-560,620-624` |
| JSON-typed per-key values for array/object | `src/tool_command.rs:514-560` (`coerce_value`), `:587-617` (`finalize_arrays` no-op on real arrays — already true) |
| Error-contract wording | same file; unit tests in `src/tool_command/tests.rs` (one test per table row) |
| Help: enum values, items shape, generated `--args -` example for tools with non-scalar params, unified footer | `src/mcp/tools/mod.rs:85-160` (`render_tool_cli_help`); list footer `src/tool_command.rs:693-697`; trailer `src/cli/help.rs:76-106` |
| Skill/catalog/steering rewrite | Section 6 |
| Eval corpus + harness extensions | Section 7 |
| Contract tests pinning taught text ↔ parser | `tests/agent_suite/agent_test.rs` (exists for hermes `tools.py`, `:1117-1120`) + the shared/plugin skill contract tests PR #286 already exercises |

---

## 6. Discoverability: make the taught model identical to the parser

The principle: **an agent should see the same one-paragraph contract in
every place it can learn from — steering, skill, catalog, `--help`, and
error messages — and that contract should be Section 5.1 verbatim.**

1. **Codex/Cursor steering** (`src/hooks/steering.rs:137-143`, mirrored
   in `src/agents/mod.rs:816` and `src/agents/hermes/templates/skill.md:15`):
   replace "`tracedecay tool <name> --key value`" with:
   > every tool is also a shell command: `tracedecay tool <name> --args
   > '<json>'` — the same JSON arguments object as the MCP tool; pipe it
   > via `--args -` (heredoc) when it has quotes/newlines. `tracedecay
   > tool` lists tools; `tracedecay tool <name> --help` shows parameters.
   Keep it to ~2 lines; steering is paid for in every session.
2. **`using-the-cli` SKILL.md**: invert the Invocation section — JSON-first
   with the heredoc example as the canonical form; `--key value` follows
   as "quick scalar calls"; document `--dry-run` for pre-flighting edit
   tools; keep the discovery flow and retrieval sections as-is.
3. **`tool-arg-catalog.md`**: fix the actively-wrong rows *immediately*
   (in PR #286): `multi_str_replace` → ``--args -`` heredoc example;
   `body` → `--symbol`. Restructure each row to show *the MCP argument
   names* (which are the `--key` names modulo kebab-case) plus one
   ready-to-copy example per hard-shape tool. Add a top "Invocation
   grammar" that is Section 5.1's paragraph.
4. **Per-tool `--help`** (`render_tool_cli_help`): append enum values
   (`one of: complexity | lines | …`) and array item shapes
   (`array of [old, new] string pairs` derived from `items`); for any tool
   with an array/object param, emit a generated example:
   ```
   Example:
     tracedecay tool multi_str_replace --args - <<'JSON'
     {"path": "<path>", "replacements": [["<old_str>", "<new_str>"]]}
     JSON
   ```
   (constructed mechanically from `properties` + `required` — placeholders
   from the property names). Unify the three footers to the same two
   lines (payload forms; `@`/`@-` per-key).
5. **Other skills / Cursor rule / prompt rules** (the dozen `--key value`
   citations in Section 3.4): mechanical rewrite to "`tracedecay tool
   <name>` (see `tracedecay:using-the-cli`)" — stop repeating the grammar
   in surfaces that can drift; the skill is the single source.
6. **Contract tests**: extend the plugin/shared skill contract tests (run
   in PR #286's test list) to assert the steering string, SKILL.md, and
   catalog all contain the `--args -` form and do **not** teach per-key
   for `replacements`, so the taught model cannot silently drift from the
   parser again.

---

## 7. Eval plan

Goal: measure, with real Sonnet and Codex sessions in the hermetic
harness, whether an agent that must fall back to the CLI can construct
correct tool calls across the hard shapes — and whether it self-corrects
when it can't — before and after the changes.

### 7.1 Harness (existing, verified)

`eval/hermetic/run.sh` builds this worktree's binary, stages it at a
non-cargo path, installs the plugin into an isolated
`CLAUDE_CONFIG_DIR`/`CODEX_HOME`/`TRACEDECAY_DATA_DIR`, indexes a target
project, then runs each corpus line via `claude -p … --model sonnet`
(`run.sh:244-247`) or `codex exec --json` (`run.sh:252-258`) and scores
the isolated transcript with `score.py`. A scenario passes when every
`expected_tools` fragment appears among MCP tool names, every
`expected_cli` fragment appears among captured shell command strings, and
no `anti_tools` appear (`score.py:203-231`; `eval/hermetic/README.md:87-110`).
The existing two-scenario corpus (`corpora/tool-args-ergonomics.jsonl`)
already demonstrates the MCP-first vs CLI-fallback pattern; it stays
untouched as a continuity check.

Key property exploited below: for Claude, the Bash `tool_use` input
contains the **full command text including heredoc bodies**
(`score.py:137-143`), so fragment matching sees inside `--args -`
payloads; Codex command strings are captured equivalently
(`score.py:147-167`).

### 7.2 Harness extensions (small, additive)

1. **`verify_cmd`** (per-scenario, optional): a shell command run by
   `run.sh` in the scenario's `project_dir` *after* the agent session,
   with the env's staged binary first on PATH; its exit status is passed
   to `score.py` (new `--verify-status` arg) and folded into `pass` as
   `verify_pass`. This is how edit-tool scenarios assert *effect* (file
   actually contains the replacement) rather than command shape.
2. **Attempt counting**: `score.py` gains `tool_cmd_attempts` = number of
   captured commands containing `tracedecay tool <name-fragment>`
   (per-scenario `attempt_tool` field), and `self_corrected` =
   `pass && tool_cmd_attempts > 1`. This turns "did the corrective error
   teach the retry" into a metric without a smarter judge.
3. **Reset between reps**: `run.sh run` gains `--reps N` (re-run the
   corpus N times, appending to `results.jsonl` with a `rep` field);
   `verify_cmd` scenarios provide a `setup_cmd` to restore fixture state
   (e.g. `git checkout -- lib.rs` in the fixture project) run before each
   rep.

Corpus schema additions documented in `eval/hermetic/README.md:87-110`.

### 7.3 Fixture

A tiny dedicated fixture project (3–4 files, committed under
`eval/hermetic/fixtures/tool-args/`, copied into the env and indexed by
`run.sh index`) rather than the tracedecay repo itself: edit scenarios
must mutate files deterministically, and `verify_cmd`/`setup_cmd` need
stable content. One file carries a function whose body contains a comma,
both quote characters, and a `$` — the quoting gauntlet. A second
registered project (one file) is indexed to exercise `project_selector`.
A >128 KiB `cargo-output.txt` fixture feeds the argv-cap scenario.

### 7.4 Corpus: `eval/hermetic/corpora/tool-args-agent-path.jsonl`

All prompts begin from the same fiction the existing corpus uses
("Assume the TraceDecay MCP server is unavailable; use the tracedecay CLI
fallback"), include `providers: ["sonnet","codex"]`, and anti-tools ban
raw DB access (`sqlite3`, `.tracedecay/`). Fragment expectations are
chosen to be *shape-agnostic* where multiple correct forms exist (a
fragment like `"fan_in"` matches per-key, inline JSON, and heredoc alike);
effects are verified where the tool has one.

| id | Forces | Prompt sketch | Pass signal |
|---|---|---|---|
| `ap-array-of-pairs` | array of `[old,new]` pairs | apply two replacements in `lib.rs`, one new string containing `', '` and `$x` | `expected_cli: ["tracedecay tool multi_str_replace"]`; `verify_cmd`: grep the file for both new strings; `attempt_tool: multi_str_replace` |
| `ap-multiline-string` | multi-line string param | insert a 5-line doc comment (contains both quote types) above a named function via `insert_at` | `expected_cli: ["tracedecay tool insert_at"]`; `verify_cmd`: grep for a sentinel line |
| `ap-nested-object` | object param | search for symbol `zeta` in *the other registered project* using a project selector | `expected_cli: ["project_selector"]` or `["project-path"]` (either correct spelling); success text mentions the hit |
| `ap-enum-param` | enum | "compute inequality of fan in per file" (phrasing tempts `fanin`/`fan-in`) | `expected_cli: ["tool gini","fan_in"]` |
| `ap-whole-payload-stdin` | argv cap + stdin | diagnose the provided >128 KiB `cargo-output.txt` — "pipe the file, do not paste it inline" | `expected_cli: ["tool diagnose","--args"]`; passing `@`/`-`/file all count (fragment `--args`) |
| `ap-typo-recovery` | unknown-key correction | "search for `gamma` capping results with the `max_results` option" (real param: `limit`) | `expected_cli: ["--limit"]` or `["\"limit\""]`; **baseline expectation: fail silently** (agent uses `--max-results`, sees success, never corrects); after: corrective error → `self_corrected` |
| `ap-help-one-shot` | discoverability | construct a `fact_store` add for a given decision text using only `--help` (catalog withheld by prompt) | `expected_cli: ["tool fact_store"]`; `verify_cmd`: `tracedecay tool fact_store --args '{"action":"search","query":…}' --json | grep <sentinel>`; `tool_cmd_attempts ≤ 3` |
| `ap-dry-run-preflight` *(candidate arm only, Phase 1+)* | `--dry-run` | validate a `multi_str_replace` payload **without applying it**, then apply | `expected_cli: ["--dry-run"]`; `verify_cmd` checks final content |

Eight scenarios × 2 agents. `ap-dry-run-preflight` is excluded from the
baseline arm (flag doesn't exist there); it establishes the Phase-1
affordance is discoverable from help alone.

### 7.5 Protocol: baseline vs after

Two arms, identical corpus, identical fixture, same models:

- **Arm A (baseline)**: binary + plugin from `master` (pre-#286 docs), via
  `run.sh setup` on a master checkout.
- **Arm B (candidate)**: this branch (per phase: B0 = docs/steering only;
  B1 = + validation/`--dry-run`; B2 = + help generation).

Per arm: `setup` → `index` (fixture + second project) → `run --corpus
tool-args-agent-path.jsonl --reps 3` for `--agent claude --model sonnet`
and `--agent codex`. 8 scenarios × 2 agents × 3 reps × 2 arms ≈ 96 short
sessions — same manual, cost-gated posture as the existing harness (no
CI; `eval/run_real_model.py` sets the precedent for explicit cost
consent). Record per-arm `summary.md` pass rates and mean
`tool_cmd_attempts`; store both as durable facts per the README's
post-merge protocol (`eval/hermetic/README.md:144-158`).

**Success bar:** Arm B ≥ Arm A on every scenario (majority-of-3 reps);
the four trap scenarios (`array-of-pairs`, `multiline`, `enum`,
`typo-recovery`) move from expected-fail/flaky to ≥ 2/3 pass per agent;
mean attempts on hard shapes ≤ 2; zero silent-failure passes (a
`verify_cmd` failing while fragments pass counts as fail — that
combination *is* the silent-failure detector).

### 7.6 Hypotheses (falsifiable, grounded in Section 4)

| Scenario | Baseline prediction (why) | After prediction |
|---|---|---|
| `ap-array-of-pairs` | Mostly fail or multi-attempt: catalog/help steer to per-key `--replacements` (F2); handler error doesn't mention `--args`; some agents recover by inventing `--args`, then fight quoting (F5) | 1–2 attempts via heredoc; corrective error catches per-key strays |
| `ap-multiline-string` | Flaky: inline quoting breaks (F5); some agents write a temp file + `@file` (fine, passes) | heredoc first-shot |
| `ap-nested-object` | Fail: per-key object arrives as string, silently ignored (F1) → wrong-project results; fragments may pass while `verify` fails — recorded as silent failure | JSON path or corrective type error |
| `ap-enum-param` | Split: `fan_in` guessable from description text, but `fan-in`/`fanin` silently accepted (F1) → wrong output, no correction | corrective enum error → self-correct ≤ 2 attempts |
| `ap-whole-payload-stdin` | Mixed: argv-cap unknown to some agents; inline attempt may hit E2BIG with an opaque OS error | taught stdin form, first-shot |
| `ap-typo-recovery` | Fail silently (F1): `--max-results` ignored, agent reports success | unknown-key error names `--limit` → self-correct |
| `ap-help-one-shot` | Multi-attempt: help lacks enum/shape info (F6) | ≤ 3 attempts with generated example in help |

If Arm B0 (docs only) already clears most of the bar, that is a
finding: the parser was adequate and the *teaching* was the bug — Phases
1–2 then stand on the remaining deltas (`typo-recovery` and
`enum` cannot pass B0; they need the validation gate).

---

## 8. Rollout

Phased, lowest-risk first; every phase independently shippable and
re-evaluated (Section 7.5 arms map to phases).

**Phase 0 — reshape PR #286 (docs + evals, zero parser changes).**
The branch's parser work (payload conventions, stdin memoization, tests)
is correct and stays. The PR grows: the Section 6 rewrites of
`using-the-cli` SKILL.md, `tool-arg-catalog.md` (fixing the actively-wrong
`multi_str_replace`/`body` rows), steering strings
(`src/hooks/steering.rs`, `src/agents/mod.rs:816`, hermes template), and
`src/cli/help.rs` trailer; the new corpus + fixtures + `verify_cmd`/
`attempts`/`--reps` harness extensions; contract-test updates pinning the
new taught text. What PR #286 *becomes*: "the CLI's agent contract is the
MCP arguments object over `--args`/stdin; docs, steering, and evals now
say so" — a docs-and-measurement PR on top of already-landed plumbing.
Back-compat: total (prose + additive harness changes only).

**Phase 1 — validation gate + corrective errors + `--dry-run`.**
`validate_tool_args`, the Section 5.5 error contract, `--dry-run`; unit
tests per contract row; re-run Arm B1. Back-compat break, intentional and
narrow: unknown keys and invalid enums now error (previously silent).
Hermes `tools.py` is safe (it sends schema-exact dicts, and injected keys
like `messages`/`storage_scope`/`hermes_home` exist in the LCM/memory
schemas — verify against `PROFILE_SCOPED_LCM_TOOLS` handling in
`src/tool_command.rs:50-78` during implementation; if any injected key is
absent from a schema, add it to the schema rather than weakening the
gate). Call the break out in the changelog.

**Phase 2 — help generation.** Enum values, item shapes, generated
`--args -` example, unified footers (`render_tool_cli_help`,
`src/mcp/tools/mod.rs:85-160`). Re-run Arm B2 (expect `ap-help-one-shot`
delta). No behavior change.

**Phase 3 (optional, human polish) — per-key repairs.** `--key=value`,
JSON-typed per-key array/object values, single-dash guard (Section 5.4).
Lowest urgency: by now agents are on the JSON path; this serves humans.
Each is individually testable in `src/tool_command/tests.rs`.

Explicit non-goals: no new top-level command, no raw JSON-RPC mode, no
clap dynamic subcommands, no removal of positionals/`--key value`/
`@file`, no MCP protocol changes.

---

## 9. Open questions and risks

1. **Strictness vs unknown consumers.** Phase 1's unknown-key/enum
   rejection could break third-party scripts relying on silently-ignored
   params. Judged acceptable (that silence is a latent bug), but if
   telemetry or issue reports say otherwise, the fallback is
   downgrade-to-warning on stderr for one release. Decide at Phase 1
   review.
2. **Schemas that intentionally accept extra keys.** The validation gate
   assumes `properties` is exhaustive. Audit for handlers reading
   undeclared keys (the Hermes-injected `messages`/`hermes_home` pattern
   is the known case); fix schemas, not the gate. Risk: one missed case
   bricks a working integration — mitigated by running the full
   `agent_suite` and hermes plugin tests in Phase 1.
3. **Steering token budget.** The 2-line steering rewrite is
   cost-neutral, but adding a heredoc example to session-injected text is
   not free at fleet scale. Current stance: example lives in `--help` and
   the skill, one-sentence rule in steering. Revisit if `ap-*` scenarios
   show agents not finding the heredoc form from steering alone.
4. **Eval judge fidelity.** Fragment matching can false-pass (command
   emitted but failed) — `verify_cmd` closes this for edit tools, but
   read-only scenarios (`ap-enum-param`, `ap-nested-object`) still lean on
   fragments; a transcript-level LLM judge is out of scope (README's
   stated philosophy: the harness guarantees isolation, not sophisticated
   grading). Accepted; attempts + fragments + effects triangulate well
   enough for a before/after signal at N=3.
5. **Windows.** No daemon socket; dispatch already falls back in-process
   (`src/tool_command.rs:206-209,227-267`). stdin/heredoc guidance holds
   in POSIX-ish shells agents use; PowerShell heredoc syntax differs —
   the docs should show the `--args payload.json` form as the
   portable alternative. Low priority; agent fleet is overwhelmingly
   POSIX.
6. **`--json` vs `--format json`** (F8): consolidating the two knobs is
   out of scope here (it spans handler output shaping); document the
   distinction in the skill for now, and consider folding `--format` into
   the help footer text.
7. **Error-variant taxonomy.** Whether to add a `TraceDecayError::Usage`
   variant or keep `Config` with reworded messages — cosmetic; decide in
   Phase 1 code review.
8. **External ossification.** Users' own CLAUDE.md files and memory facts
   teach `--key value` (this repo's owner included). Nothing we ship
   un-teaches those; the corrective errors are the safety net that
   retrains stale habits in one round-trip. That is precisely why the
   error contract, not the happy path, is the highest-leverage surface.

---

## Appendix A — empirical evidence log

Environment: this worktree's `target/debug/tracedecay` (0.0.29, commit
`14ae43de`), `TRACEDECAY_DATA_DIR` isolated to a scratch dir, throwaway
project `tiny-proj/lib.rs` (`fn alpha() {}\nfn beta() { alpha(); }`)
indexed via `tracedecay init` (1 file, 3 nodes). Boilerplate note lines
elided.

| # | Command | Output (verbatim, trimmed) |
|---|---|---|
| A.1 | `tool search --query=foo` | ``Error: config error: flag `--query=foo` requires a value`` |
| A.2 | `tool search --query=foo --json` | ``Error: config error: missing required parameter `--query` for tool `search` `` (the unknown key `query=foo` consumed `--json` as its value, then both vanished) |
| A.3 | `tool context "how" --include-code` | ``Error: config error: flag `--include-code` requires a value`` |
| A.4 | `tool context "how" --include-code --json` | ``Error: config error: --include-code: expected a boolean (true/false), got `--json` `` |
| A.5 | `tool search -query foo` | ``Error: config error: unexpected positional argument(s): foo — use --key value flags or run `tracedecay tool search --help` `` (`-query` silently bound to `query`) |
| A.6 | `tool search --query gamma --limt 2 --json` | Succeeds; returns results with default limit — `--limt` silently forwarded and ignored |
| A.7 | `tool multi_str_replace --path lib.rs --replacements '[["alpha","gamma"]]'` (the catalog-taught shape) | ``Error: config error: each replacement must be an array of exactly 2 strings`` — comma-split mangled the JSON before the handler saw it; file untouched. Same edit via `--args '{"path":"lib.rs","replacements":[["alpha","gamma"]]}'` parses correctly (handler then rightly rejects the ambiguous 2-site match) |
| A.8 | `tool gini --metric bogus` | Succeeds: `gini: 0 … metric: bogus` — invalid enum accepted end-to-end. `tool gini --help` shows `--metric  string  optional  Metric to measure inequality for (default: complexity)` — allowed values not shown |
