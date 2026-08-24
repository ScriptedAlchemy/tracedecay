# `tracedecay tool` argument catalog

Every MCP tool is also a shell command. The argument surface is the tool's
**MCP `arguments` object** — pass it whole with `--args`; spell top-level fields
as `--key value` only for quick scalar calls. The source of truth is always
`tracedecay tool <name> --help`; regen this file if a tool's parameters drift.

## Invocation grammar

> The arguments of `tracedecay tool <name>` are the tool's MCP `arguments`
> object. Pass it whole with `--args` (inline JSON, `-` for stdin — use a quoted
> heredoc, or a file path). Or, for quick scalar calls, spell top-level fields
> as `--key value` flags; values are interpreted by the tool's schema, and
> anything that isn't a scalar is JSON.

- **Canonical form — `--args -` with a quoted heredoc.** No shell escaping, no
  argv-length limit, byte-exact MCP parity. Use it whenever the payload has
  quotes, newlines, or any non-scalar value.
- **`--args '<json>'`** — inline JSON, only when the object is short and
  contains no single quotes.
- **`--args payload.json`** (or `--args @payload.json`)** — read from a file;
  use for payloads near or over the ~128 KiB per-argument shell limit.
- **`--key value`** — quick scalar-only calls (`search --query "x" --limit 5`).
  `--key=value` works too. Enum/array/object params are safer via `--args`.
- Tool names work with or without the `tracedecay_` prefix
  (`tool search` ≡ `tool tracedecay_search`); dashes and underscores are
  interchangeable in both tool names and `--key` names.
- `--json` prints the raw JSON result; `--format json` is the per-tool payload
  switch for read tools that offer it. `--project <path>` pins the project root.
- `--dry-run` parses, validates, and prints the resolved arguments object
  without dispatching — pre-flight destructive edits with it.
- Truncated responses return a `handle` envelope — dereference with
  `tracedecay tool retrieve --handle rh_…`.

## Reserved / global flags

`--args <json|-|@file|file>`, `--dry-run`, `--json`, `--project <path>`,
`-h`/`--help`. Per-key values starting with `@` are read from that file
(`--new-source @/tmp/body.txt`); `@-` reads stdin.

## Tool categories

`tracedecay tool` (no name) lists every tool grouped by category:
`always-loaded`, `analysis`, `edit`, `git & history`, `graph`, `health`,
`info`, `memory & session`, `workflow`.

## Hard-shape tools (copy-paste examples)

These tools have non-scalar parameters (arrays of arrays/objects, nested
objects, enum constraints, or large multi-line strings) where the heredoc form
is the sane path. Parameter names are the MCP argument names (the `--key`
modulo kebab-case).

### `multi_str_replace` — array of `[old, new]` pairs

```bash
tracedecay tool multi_str_replace --args - <<'JSON'
{
  "path": "src/lib.rs",
  "replacements": [
    ["old_first", "new_first"],
    ["old, with 'quotes'", "new with $body"]
  ]
}
JSON
```

Required: `path`, `replacements` (an array of exactly-2-element `[old_str,
new_str]` string pairs). All replacements must match exactly once or the whole
edit aborts. Prefer the heredoc: it survives quotes, `$`, and newlines that
would fight shell quoting in a per-key value.

### `insert_at` — multi-line `content`

```bash
tracedecay tool insert_at --args - <<'JSON'
{
  "path": "src/lib.rs",
  "anchor": "fn alpha() {",
  "before": true,
  "content": "/// Doc comment with both 'single' and \"double\" quotes,\n/// spanning multiple lines.\n"
}
JSON
```

Required: `path`, `anchor` (unique string or 1-indexed line number), `content`.
Optional: `before` (boolean; default `false` = insert after the anchor line).
For the body text alone, `--content @/tmp/block.txt` also works.

### `replace_symbol` — multi-line `new_source`

```bash
tracedecay tool replace_symbol --args - <<'JSON'
{
  "symbol": "mymod::do_thing",
  "new_source": "fn do_thing() -> u32 {\n    42\n}\n"
}
JSON
```

Required: `symbol` (prefer a fully qualified name), `new_source` (full
replacement source including the symbol's own declaration line).

### `ast_grep_rewrite` — ast-grep `pattern` + `rewrite`

```bash
tracedecay tool ast_grep_rewrite --args - <<'JSON'
{
  "path": "src/lib.rs",
  "pattern": "println!($$$)",
  "rewrite": "eprintln!($$$)"
}
JSON
```

Required: `path`, `pattern` (SGPattern syntax), `rewrite`. Advertised only when
the host `ast-grep` CLI is available.

### `gini` — `metric` enum

```bash
tracedecay tool gini --metric fan_in
# or
tracedecay tool gini --args '{"metric":"fan_in","scope":"file"}'
```

`metric` is one of: `complexity | lines | fan_in | fan_out | members`. `scope`
is `file | symbol`. An invalid enum value is rejected with the allowed list.

### Cross-project queries — `project_selector` (object)

`search`, `context`, `callers`, and other read tools accept
`project_selector` (object), `project_id` (string), or `project_path`
(string) to target another registered project:

```bash
tracedecay tool search --args - <<'JSON'
{
  "query": "zeta",
  "project_selector": {"project_path": "/abs/path/to/other/project"}
}
JSON
```

Find a project id/path with `tracedecay projects list` / `projects search`.

### Large payloads — over the ~128 KiB shell limit

```bash
tracedecay tool diagnose --args @./cargo-output.txt
# or pipe it:
tracedecay tool diagnose --args - < ./cargo-output.txt
```

Linux caps a single argv string at 128 KiB (`MAX_ARG_STRLEN`); a heredoc body,
a file, or stdin sidesteps that. `diagnose` takes `cargo_output` (string).

## Scalar tools (quick `--key value` form)

For tools whose parameters are all strings/numbers/booleans, `--key value`
flags are fine. Parameter names below are the MCP argument names in kebab-case.

| Tool | Required flags | Common optional flags |
|---|---|---|
| `search` | `--query` | `--limit`, `--format` |
| `context` | `--task` | `--include-code`, `--max-nodes`, `--mode` |
| `body` | `--symbol` | `--limit` |
| `callers` / `callees` | `--node-id` (from a prior search/context hit) | `--max-depth` |
| `impact` | `--node-id` | `--max-depth` |
| `signature` | `--qualified-name` (or `--node-id`) | — |
| `signature_search` | — | `--returns`, `--params`, `--async`, `--path`, `--limit` |
| `similar` | `--symbol` | `--limit` |
| `field_sites` | `--field` (`Struct::field`) | — |
| `constructors` | `--struct` | — |
| `rename_preview` | `--node-id` | — |
| `str_replace` | `--path`, `--old-str`, `--new-str` | — |
| `diagnostics` | — | `--scope`, `--path`, `--maximum-diagnostics`, `--cursor` |
| `diagnose` | `--cargo-output` | `--severity`, `--include-callers` |
| `affected` | `--files` | — |
| `diff_context` | `--files` | — |
| `pr_context` | `--base-ref`, `--head-ref` | — |
| `fact_store_search` | `--query` | `--min-trust` |
| `fact_store_list` | — | `--limit`, `--min-trust` |
| `message_search` | `--query` | `--provider`, `--limit` |
| `retrieve` | `--handle` | — |

Notes on the scalar list: array-of-strings params (e.g. `affected --files`,
`diff_context --files`, `file_metadata --files`) accept repetition
(`--files a --files b`) or comma-splitting (`--files a,b`); JSON
(`--files '["a","b"]'`) also works. `str_replace`'s `--old-str`/`--new-str`
accept `@file` / `@-` for multi-line bodies.

## Non-tool subcommands

`tracedecay --help` lists the rest: `init`, `sync`, `status`, `doctor`,
`daemon`, `sessions`, `dashboard`, …. Each carries its own `Examples:` and
`Related:` sections — read those before improvising flags.
