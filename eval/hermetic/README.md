# Hermetic eval harness

Triggering evals must exercise the tracedecay build **under development in this
worktree** — its binary *and* its plugin bundle — never the system-installed
`tracedecay` and never the user's real Claude Code config. Live concurrent
sessions depend on the real `~/.claude`, `~/.tracedecay`, and
`~/.cargo/bin/tracedecay`, so the harness touches none of them.

```bash
# One-shot: build + isolate + install + index + one trivial scenario.
eval/hermetic/run.sh smoke --agent claude --model sonnet --debug --keep
eval/hermetic/run.sh smoke --agent codex --debug --keep

# Full corpus against a reusable env:
ENV=$(eval/hermetic/run.sh setup --agent claude --debug)
eval/hermetic/run.sh index --env-dir "$ENV" --project /path/to/repo
eval/hermetic/run.sh run   --agent claude --env-dir "$ENV" --corpus my-corpus.jsonl --model sonnet
eval/hermetic/run.sh teardown --env-dir "$ENV"
```

## Why a naive PATH override is not enough

An eval session launched via `claude -p` resolves tracedecay **three** ways:

1. **MCP server command** — the plugin registers an MCP server whose command is
   the bare string `tracedecay`, resolved via `PATH` at session start.
2. **Hook commands** — baked as **absolute paths** at install time. The
   installer substitutes `__TRACEDECAY_BIN__` with a concrete path
   (`src/agents/claude.rs`), so a `PATH` override does *not* affect already
   installed hooks.
3. **Plugin bundle** — skills / hooks / agents JSON that Claude Code loads from
   its plugin marketplace under the config dir.

A `PATH` override alone only fixes (1). The harness must control all three.

## What the harness isolates (verified mechanisms)

| Concern | Lever | Evidence |
| --- | --- | --- |
| Claude config, transcripts, plugin bundle | `CLAUDE_CONFIG_DIR` | Smoke test: a throwaway `CLAUDE_CONFIG_DIR` gets its own `projects/`, `sessions/`, `.claude.json`; the session transcript lands at `<config>/projects/<slug>/<session_id>.jsonl`. |
| Codex config, auth copy, plugin cache, sessions | `CODEX_HOME` | Codex mode copies only `auth.json`, installs the staged plugin source, runs `codex plugin add tracedecay@personal`, and stores `codex exec --json` output under `<env>/results/`. |
| Where the installer writes the plugin | `HOME` (installer uses `home_dir()` → `$HOME`, then `$HOME/.claude`) | `src/agents/mod.rs::home_dir()` reads `$HOME`; `src/agents/claude.rs` writes `ctx.home/.claude/plugins/marketplaces/tracedecay`. |
| tracedecay graph/data home | `TRACEDECAY_DATA_DIR` | `src/config.rs::user_data_dir()` returns `$TRACEDECAY_DATA_DIR` when set, else `~/.tracedecay`. |
| tracedecay daemon socket | derives from the data dir; also pinned via `TRACEDECAY_DAEMON_SOCKET` | `src/daemon/service.rs::default_socket_path()` = `tracedecay_data_dir()/daemon.sock`, overridable by `TRACEDECAY_DAEMON_SOCKET`. Isolating the data dir already isolates the socket, so the harness never fights the real daemon. |
| Which binary the installer bakes | **staged copy of the dev binary at a non-cargo-target path** | `src/agents/mod.rs::which_tracedecay_from()` deliberately **refuses** a path under a cargo target dir (`target/{debug,release}`) and falls back to `PATH` — which would bake the *system* binary. See below. |
| Auth for `claude -p` | copy `~/.claude/.credentials.json` into the isolated config (or `ANTHROPIC_API_KEY`) | Smoke test: without it the isolated session prints `Not logged in`; with the copied credential it returns a real result and `session_id`. |

`HOME` and `CLAUDE_CONFIG_DIR` are pointed at the **same** physical directory
(`<env>/home/.claude`) so the installer's `$HOME/.claude` writes and Claude
Code's `CLAUDE_CONFIG_DIR` reads refer to one bundle.

### The cargo-target-binary trap (the crux)

`which_tracedecay()` treats any binary living under a cargo target dir as
ephemeral and **will not** bake it into hooks; it prefers a `PATH` match
instead. So running `target/release/tracedecay install` directly would silently
bake the **system** `tracedecay`, defeating the whole point.

The harness sidesteps this by copying the freshly built binary to
`<env>/bin/tracedecay` (a stable, non-cargo location) and running the installer
from **that** copy with `<env>/bin` first on `PATH`. Then:

* `current_exe` is the staged copy → baked into hook commands (fixes #2), and
* the MCP `tracedecay` command resolves to the staged copy via `PATH` (fixes #1),
* the plugin bundle is deployed from this worktree's `plugin/` dir (fixes #3).

`setup` asserts the staged path actually appears in the baked hook JSON and
warns loudly if it does not.

## Env dir layout

```
<TMPDIR>/eval-env-<timestamp>-<pid>/
  bin/tracedecay          staged dev binary (baked into hooks + first on PATH)
  home/                   fake $HOME
  home/.claude/           == CLAUDE_CONFIG_DIR (plugin bundle, transcripts)
  home/.codex/            == CODEX_HOME (auth copy, plugin cache, sessions)
  tracedecay-data/        == TRACEDECAY_DATA_DIR (graph db, daemon.sock, logs)
  results/                results.jsonl, summary.md, per-scenario json/stderr
  env.sh                  sourceable export block (for reuse and manual debugging)
```

`--keep` preserves the env for inspection; otherwise a freshly created env is
removed on exit. `teardown --env-dir` refuses to delete anything that is not an
`eval-env-*` dir under `$TMPDIR`.

## Corpus schema

One JSON object per line (the schema used by the session scratchpad corpora):

```json
{"id":"ev-001","category":"context","project_dir":"/abs/path/to/repo",
 "prompt":"...","expected_tools":["tracedecay_context"],
 "expected_cli":["tracedecay tool diff_context","--args -"],
 "anti_tools":["Grep","Glob"],"providers":["sonnet","codex"],"success":"..."}
```

Required fields: `id`, `category`, `project_dir`, `prompt`, `expected_tools[]`,
`expected_cli[]`, `anti_tools[]`, `providers[]`, `success` (one-sentence pass
criterion).

Optional per-scenario fields:

- **`setup_cmd`** — shell command run in `project_dir` before the agent session
  (restore fixture state between reps).
- **`verify_cmd`** — shell command run in `project_dir` after the session with
  the staged dev binary first on `PATH`; non-zero exit fails the scenario.
- **`attempt_tool`** — tool-name fragment; captured commands containing
  `tracedecay tool` plus this fragment are counted as `tool_cmd_attempts`.

`project_dir` may be an absolute path or **`fixture:<name>`**, resolved at run
time to `<env>/fixtures/<name>` (staged copies of `eval/hermetic/fixtures/*`).
Use `run.sh fixtures --env-dir "$ENV"` to copy and index fixtures without a
full corpus run; `run --reps N` re-stages fixtures automatically before each
rep after the first.

`run` executes each `prompt` through the selected `--agent` inside
`project_dir` (falling back to the indexed default project if the dir is
missing). Claude mode recovers the `session_id` and reads the isolated
transcript. Codex mode scores the captured `codex exec --json` stream.
`score.py` classifies MCP tools and CLI commands separately.

A scenario **passes** when every `expected_tools` fragment appears in MCP tool
names, every `expected_cli` fragment appears in captured command strings, and no
`anti_tools` appear. When `verify_cmd` is set, its exit status is also
required (`verify_pass`). If no expectations are listed, the fallback pass
criterion is at least one tracedecay MCP tool and no anti-tool use. This is a
deliberately simple end-state judge — the harness exists to guarantee
*isolation*, not to be a sophisticated grader; layer a stricter judge on top of
`results.jsonl` if needed.

Each scored row in `results.jsonl` also records derived fields: **`verify_pass`**
(whether `verify_cmd` succeeded, or `null` if unset), **`tool_cmd_attempts`**
(matching CLI invocations for `attempt_tool`), **`self_corrected`**
(`pass && tool_cmd_attempts > 1`), and **`rep`** (repetition index when
`run --reps N` is used).

The args-ergonomics corpus compares MCP-first behavior with CLI fallback:

```bash
ENV=$(eval/hermetic/run.sh setup --agent claude --debug)
eval/hermetic/run.sh index --env-dir "$ENV" --project /path/to/tracedecay-worktree
eval/hermetic/run.sh run --agent claude --env-dir "$ENV" \
  --corpus eval/hermetic/corpora/tool-args-ergonomics.jsonl --model sonnet

ENV=$(eval/hermetic/run.sh setup --agent codex --debug)
eval/hermetic/run.sh index --env-dir "$ENV" --project /path/to/tracedecay-worktree
eval/hermetic/run.sh run --agent codex --env-dir "$ENV" \
  --corpus eval/hermetic/corpora/tool-args-ergonomics.jsonl
```

Outputs land in `<env>/results/`: `results.jsonl` (one scored object per
scenario) and `summary.md` (pass count + per-scenario table).

## What it cannot isolate / limitations

- **Model non-determinism.** Tool-use counts vary run to run; treat pass/fail as
  a signal over a corpus, not a single scenario.
- **The `tracedecay init` index cost.** Indexing the tracedecay repo is not
  free; reuse an env dir with `--keep` across corpus runs.
- **Network / model backend.** Evals hit the real Anthropic API using the copied
  credential. There is no offline mode.
- **Codex auth.** Codex mode copies only `~/.codex/auth.json` into the isolated
  `CODEX_HOME`. If Codex changes auth storage, `codex exec` will fail closed in
  the eval env instead of mutating the user's real config.
- **Global cargo caches** (`~/.cargo/registry`) are shared — only the *output*
  (`CARGO_TARGET_DIR=<worktree>/target`) is worktree-scoped. The build cannot
  corrupt the user's install because it never writes to `~/.cargo/bin`.

## Post-merge re-eval protocol

After the plugin-suite changes merge, re-run to compare against the baseline
facts recorded in tracedecay memory:

1. **Rebuild** from the merged checkout: `run.sh setup` (drop `--debug` for a
   representative release build).
2. **Re-index** the same target project into the fresh env.
3. **Rerun the same corpus** at the same `--model`.
4. **Compare** the new `summary.md` pass rate and per-scenario tracedecay-vs-
   native counts against the baseline. Store the new baseline as a durable fact
   (`tracedecay_fact_store`) so future regressions are visible.

Because every run is hermetic, differences between two runs are attributable to
the code change (plus model noise), not to drift in the user's real environment.
