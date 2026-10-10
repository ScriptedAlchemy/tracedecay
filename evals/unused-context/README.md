# Unused returned context

`tracedecay sessions unused-context` walks hydrated LCM session history and
reports, per tool, how many returned tokens later turns opened, edited, quoted,
or cited versus ignored.

The meter is the same `chars/4` heuristic MCP trailers already print. It is not
provider billing.

## Reproduce

```bash
tracedecay sessions unused-context --json --examples 3
tracedecay sessions unused-context --project-path /path/to/repo --session-limit 200
```

The command reads the enrolled project's `sessions.db` through the daemon and
LCM load. Missing session authority fails closed; an empty store reports zeros
without inventing a ratio.

## Corpus in this directory

`results.json` is a fixture-corpus run that uses production TraceDecay tool
result shapes (search, context, files, grep, callers, git_hunks) plus later
opens, edits, quotes, and cites. Re-run the same measurement against a live
profile with the command above; #3373 should use live ratios when they exist
and treat this file as the reproducible baseline.
