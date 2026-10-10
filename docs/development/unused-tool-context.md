# Measuring returned context agents never use

`scripts/measure-unused-tool-context.py` reads stored agent sessions and
reports, per `tracedecay_*` MCP tool, how many result bytes the agent later
referenced versus ignored. Token columns are stored real counts or null. It
drives output-trimming work (#3372): a tool whose results are mostly unused
is a trimming target.

## Run it

The script needs only Python 3.10+ and an installed `tracedecay` with a
running daemon. It reads history through `tracedecay tool sessions_for` and
`tracedecay tool lcm_load_session`. It never opens `.tracedecay` databases or
native transcript files.

```sh
# Sessions on every local branch and worktree of this checkout
scripts/measure-unused-tool-context.py --all-refs

# One branch, two providers, JSON next to the table
scripts/measure-unused-tool-context.py --branch master --providers claude,kimi --json target/unused-context.json

# Explicit sessions; print 3 scored calls per tool to stderr for spot checks
scripts/measure-unused-tool-context.py --session kimi:SESSION_ID --examples 3
```

The table goes to stdout and progress to stderr. `--examples N` prints up to N
ignored and N used calls per tool with path/symbol keys only (quoted source
lines are reduced to `quote:<chars>c`). Those sanitized rows are safe to cite.

## Columns

| Column | Meaning |
|---|---|
| `sessions` | Sessions that contain at least one scored call of the tool |
| `calls` | Calls paired with a stored result, including error results |
| `errors` | Calls whose result is an error (`isError`); they add no bytes |
| `calls 0 used` | Non-error calls where no result line was used |
| `rerequests` | Later same-tool calls that asked for an already-returned path/symbol/quote. Not counted as use. |
| `after cut` | Those rerequests whose original result recorded an explicit cut. `null` when cut state is unknown, never an estimated 0. |
| `bytes` | Exact UTF-8 bytes of the result text after removing the MCP envelope, including each line's real newline. Not `len(line)+1`. |
| `used bytes` / `unused bytes` | Bytes of used lines / all other lines |
| `unused %` | `unused bytes / bytes` |
| `tokens` | Stored `token_count` on the tool_result fact. `null` when missing. Tool-body `token_count` is ignored because `source_read` writes chars/4 there. Never tiktoken or an MCP trailer. |
| `used tokens` / `unused tokens` | The stored total when every line is used or every line is unused. Mixed lines are `null` (splitting would be an estimate). |
| `unused tokens %` | `unused tokens / tokens`, or `null` when either side is unmeasured |

The `Coverage` line reports, per provider, how many sessions were discovered,
loaded, empty, or unreadable (with the daemon's problem code), plus calls whose
result was never stored (`calls_without_result`). When any scored call lacks a
stored `token_count`, token columns stay `null` and the coverage object adds
`token_count_coverage` — the share of non-error calls that had a real count.
That share is the number to cite; do not fill the missing token columns.

## Matching rules

The rules are conservative: a line counts as used only on direct evidence.

1. A result is walked as physical lines, keeping each line's actual
   separator (`\n`, `\r\n`, or none). Empty text is 0 bytes. A line is used
   when one of its anchors occurs in agent-authored content recorded after
   the result. Blank and structural lines are unused.
2. Agent-authored content is the assistant's visible text and the argument
   values of every later tool call (Read, Edit, Grep, Bash, tracedecay tools,
   and so on). Hidden reasoning, user messages, and tool results are not
   evidence. JSON keys in tool arguments are not evidence, so a schema field
   such as `node_id` cannot match.
3. A line has three kinds of anchor:
   - **path**: a token with at least one `/` and a file extension. It matches
     on its last three path components, so `crates/a/src/lib.rs` matches a
     later read of `/repo/crates/a/src/lib.rs`.
   - **symbol**: an identifier of 6 or more characters that contains `_`,
     `::`, or a lowercase-to-uppercase transition (`parse_config`,
     `Foo::bar`, `SessionStore`). A qualified path also anchors its final
     segment when that segment qualifies. Matches are whole words, so
     `parse_config` does not match `parse_config_v2`.
   - **quote**: the whole line, whitespace-normalized, when it is 24 or more
     characters. It matches verbatim, for example inside an Edit
     `old_string`.
4. Novelty: an anchor that already occurs in agent-authored content before the
   result arrived, including the call's own arguments, is dropped. Echoing the
   query back never counts as use.
5. Re-request: a later call of the same `tracedecay_*` tool whose arguments
   share a novelty-filtered anchor is a `rerequest`, not use. Re-request after
   a cut is counted only when the original result records `cut` (boolean or
   `{applied: bool}`). Unknown cut state is `null`, never a guessed 0.
6. Tokens: only a stored `token_count` on the tool_result fact. A failed or
   absent count is `null`. Tool-body `token_count` is ignored. Used/unused
   tokens are filled only when every line is used or every line is unused.

These rules undercount use. An agent that acts on a fact without naming a
path, symbol, or line (for example "no callers, so it is safe") is scored as
unused. Treat the unused share as an upper bound.

## Data requirements

Only sessions whose stored history keeps both a tool call and its result can
be scored. Calls and results pair by the `invocation_id` of their canonical
facts. The `Coverage` line shows, per provider, how many sessions loaded,
came back empty, or failed, and how many calls had no stored result. Read it
before trusting a ratio: a provider that loads no sessions or stores calls
without results contributes no rows. Calls made through a shell command
(`tracedecay tool ...` inside `exec` or `Bash`) are not scored.

## Tests

```sh
python3 scripts/test-measure-unused-tool-context.py
```

## Corpus classes

#3372 ratios must come from naturally captured agent sessions discovered
through `sessions_for` and loaded through `lcm_load_session`. Constructed
or imported CLI-call transcripts test the scorer and the production tools;
they are not issue-completion evidence.

`docs/development/unused-tool-context-cli-experiment.md` (raw JSON beside
it) is one such experiment: live `tracedecay tool` calls recorded as
Cursor transcripts and imported. Do not cite its unused % on #3372 or
#3373.

Token columns stay `null` until stored `tool_result` facts carry a real
`token_count` (#3397 / draft #3400). Do not estimate. After that lands,
re-run the meter on naturally captured sessions and post the new raw
output.

The tests score synthetic transcripts and pin each rule: a read of a returned
path is used, an unreferenced result is unused, query echoes and earlier
mentions are not use, later tool output is not use, a later same-tool call
that repeats a returned path is a rerequest rather than use, a rerequest
after a recorded cut is counted and an unknown cut is null, missing token
counts are null, mixed-use results do not split tokens, JSON keys and
partial words do not match, parallel results in one row keep their own
content, error results add no bytes, and byte totals match the returned
text for empty, terminated, unterminated, and multibyte results.
