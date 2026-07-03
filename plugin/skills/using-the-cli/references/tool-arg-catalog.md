# `tracedecay tool` argument catalog

Every MCP tool is also a shell command: `tracedecay tool <name> --key value`.
This is the CLI fallback grammar plus the required flags for the
highest-traffic tools, so you can invoke them without a round-trip through
`--help`. The source of truth is always `tracedecay tool <name> --help`; regen
this file if a tool's parameters drift.

## Invocation grammar

```
tracedecay tool <name> --key value [--key value ...] [--json]
```

- Tool names work with or without the `tracedecay_` prefix
  (`tool search` ≡ `tool tracedecay_search`).
- `--key value` flags are the tool's parameters in kebab-case
  (`--max-depth 1` ↔ the `max_depth` parameter).
- `--args '{"key":"value"}'` passes a whole JSON argument object instead of
  individual flags.
- Any value starting with `@` is read from that file
  (`--new-source @/tmp/body.txt`) — use for multi-line payloads.
- `--json` prints raw JSON; `--format json` is the per-tool equivalent.
- `--project <path>` pins the project root; otherwise the nearest initialised
  project walking up from cwd is used.
- Truncated responses return a `handle` envelope — dereference with
  `tracedecay tool retrieve --handle rh_…`.

## Reserved / global flags

`--json`, `--project <path>`, `--args <json|@file>`, `-h`/`--help`.

## Tool categories

`tracedecay tool` (no name) lists every tool grouped by category:
`always-loaded`, `analysis`, `edit`, `git & history`, `graph`, `health`,
`info`, `memory & session`, `workflow`.

## Required flags for common tools

| Tool | Required flags | Common optional flags |
|---|---|---|
| `search` | `--query` | `--limit`, `--format` |
| `context` | `--task` | `--keywords`, `--include-code`, `--max-nodes` |
| `body` | `--node-id` (or `--symbol`) | — |
| `callers` / `callees` | `--symbol` (or `--node-id`) | `--max-depth` |
| `impact` | `--symbol` (or `--node-id`) | `--max-depth` |
| `signature` | `--symbol` | — |
| `signature_search` | `--query` | — |
| `similar` | `--query` | `--threshold` |
| `field_sites` | `--field` (`Struct::field`) | — |
| `constructors` | `--struct` | — |
| `rename_preview` | `--symbol` (or `--node-id`) | — |
| `str_replace` | `--path`, `--old-str`, `--new-str` | — |
| `multi_str_replace` | `--path`, `--replacements` (`[[old,new],…]`) | — |
| `insert_at` | `--path`, `--anchor`, `--content` | `--before` |
| `replace_symbol` | `--symbol`, `--new-source` | — |
| `ast_grep_rewrite` | `--path`, `--pattern`, `--rewrite` | — |
| `diagnostics` | — | `--scope`, `--name`, `--path` |
| `diagnose` | `--cargo-output` | `--severity`, `--include-callers` |
| `affected` | `--files` | — |
| `diff_context` | `--files` | — |
| `pr_context` | `--base-ref`, `--head-ref` | — |
| `fact_store` | `--action`, `--query` (for search) | `--min-trust` |
| `message_search` | `--query` | `--provider`, `--limit` |
| `retrieve` | `--handle` | — |

## Non-tool subcommands

`tracedecay --help` lists the rest: `init`, `sync`, `status`, `doctor`,
`daemon`, `sessions`, `dashboard`, …. Each carries its own `Examples:` and
`Related:` sections — read those before improvising flags.
