# TraceDecay #1281 real-project dogfood journey

Date: 2026-09-15

## Scope

This journey ran the installed `tracedecay` CLI from commit
`2314bd9a349479e965bc43030713ca2580b8b6d0` against two real TraceDecay
checkouts:

- `/fast/tmp/td-1281-dogfood-sol`, the linked worktree for this branch.
- `/fast/projects/tracedecay`, the canonical checkout used for the successful
  lexical read.

The branch is based on tip `46698151afab`. No fixture project or operator
TraceDecay data was used.

## Isolation

Every CLI command used these values:

```bash
STATE=/fast/tmp/td-1281-dogfood-state-2314bd9
BIN="$STATE/install/bin/tracedecay"
export HOME="$STATE/home"
export USERPROFILE="$STATE/home"
export XDG_CONFIG_HOME="$STATE/config"
export XDG_RUNTIME_DIR="$STATE/runtime"
export TRACEDECAY_HOME="$STATE/home/.tracedecay"
export TRACEDECAY_DATA_DIR="$STATE/profile"
export TRACEDECAY_PROFILE_DIR="$STATE/profile"
export TRACEDECAY_GLOBAL_DB="$STATE/profile/global.db"
export TRACEDECAY_DAEMON_SOCKET="$STATE/runtime/daemon.sock"
```

The isolated daemon used
`/fast/tmp/td-1281-dogfood-state-2314bd9/runtime/daemon.sock`. Its first start
refused mode `0755` on the runtime directory. After the directory changed to
mode `0700`, the daemon reported `daemon_ready`. This guard prevented fallback
to the operator daemon.

The installed artifact identified itself as:

```text
tracedecay 0.1.0-beta.37+2314bd9a349479e965bc43030713ca2580b8b6d0
```

Hauler built and installed the artifact:

- `cc-21907` ran `cargo build --release -p tracedecay-cli --bin tracedecay`.
- `cc-21908` ran `cargo install --path crates/tracedecay-cli --root /fast/tmp/td-1281-dogfood-state-2314bd9/install --locked --force` after `cc-21907`.

The earlier `cc-21881` build completed before the branch rebase. The replacement
build and install tickets `cc-21900` and `cc-21902` were killed when the Hauler
daemon stopped, so this journey did not use their artifacts.

## Typed state results

| Required state | Result | Evidence |
| --- | --- | --- |
| Missing registry | Blocked | Both shipped CLI routes reject before `tracedecay_project_list` can return its documented `status: "unavailable"` payload. |
| Stale index | Hit | Search returned `freshness.state: "possibly_stale"` with `staleness_state: "indexing"`. |
| Unavailable authority | Hit | Status returned `storage_health.generation_census.state: "unavailable"` and `reason: "authority_unavailable"`. |
| Successful real-project path | Hit | `grep` scanned 81 files and returned five matches from `/fast/projects/tracedecay`. |

### Missing registry cannot reach the tool handler

Before enrollment, the exact tool call was:

```bash
"$BIN" tool project_list \
  --project /fast/tmp/td-1281-dogfood-sol \
  --args '{"format":"json","limit":5}' \
  --json
```

It exited with code 1 before returning a JSON tool response:

```text
Error: config error: daemon tool call failed: config error: no TraceDecay index found at '/fast/tmp/td-1281-dogfood-sol': project is not enrolled in the authenticated profile; run 'tracedecay init' first
```

The projectless form also exited with code 1:

```bash
cd /fast/tmp
"$BIN" tool project_list \
  --args '{"format":"json","limit":5}' \
  --json
```

```text
Error: config error: daemon tool call failed: tracedecay_project_list requires an initialized code project
```

The catalog describes `project_list` as a profile registry read, and its handler
has a typed missing-registry result. The shipped binding still requires an
active initialized project. That routing requirement makes the typed result
unreachable through the CLI or MCP before enrollment.

### Enrollment and stale index

The linked worktree enrolled through the shipped command:

```bash
"$BIN" init /fast/tmp/td-1281-dogfood-sol --fresh
```

```text
initialized /fast/tmp/td-1281-dogfood-sol; daemon code-index reconciliation requested
```

The first search used the whole MCP argument object:

```bash
"$BIN" tool search \
  --project /fast/tmp/td-1281-dogfood-sol \
  --args '{"query":"CodeIndexWorktreeFreshnessV1","limit":5,"format":"json"}' \
  --json
```

Recorded response shape:

```json
{
  "status": "unavailable",
  "reason": "linked_worktree_disabled",
  "code_generation": null,
  "freshness": {
    "state": "possibly_stale",
    "indexing": {
      "staleness_state": "indexing",
      "rebuild_in_flight": true,
      "reason": "linked_worktree_disabled"
    }
  },
  "coverage": {
    "exact": {"status": "unavailable", "reason": "linked_worktree_disabled"},
    "lexical": {"status": "unavailable", "reason": "linked_worktree_disabled"},
    "graph": {"status": "unavailable", "reason": "linked_worktree_disabled"}
  }
}
```

This response hit the required stale-index state on the requested real
worktree. The linked-worktree policy prevented a successful lexical or graph
read there, so the successful read used the canonical real checkout.

### Unavailable authority

The cold status call was:

```bash
"$BIN" tool status \
  --project /fast/tmp/td-1281-dogfood-sol \
  --args '{"include_branch_diagnostics":false,"format":"json"}' \
  --json
```

Recorded response shape:

```json
{
  "code_index_freshness": {
    "status": "warming",
    "worktree": {
      "staleness_state": "indexing",
      "coverage": "partial_refresh_in_progress",
      "rebuild_in_flight": true
    }
  },
  "graph_statistics": {
    "state": "unavailable",
    "reason": "exact_scope_generation_not_ready"
  },
  "storage_health": {
    "generation_census": {
      "state": "unavailable",
      "reason": "authority_unavailable"
    }
  }
}
```

The command exited with code 0 and preserved the unavailable authority as a
typed state instead of returning zero graph counts.

### Successful lexical path on the canonical checkout

The canonical checkout enrolled into the same isolated profile:

```bash
"$BIN" init /fast/projects/tracedecay
```

```text
initialized /fast/projects/tracedecay; daemon code-index reconciliation requested
```

The successful lexical command was:

```bash
"$BIN" tool grep \
  --project /fast/projects/tracedecay \
  --args '{"pattern":"CodeIndexWorktreeFreshnessV1","fixed_strings":true,"case_sensitive":true,"path_glob":"crates/**/*.rs","max_results":5,"format":"json"}' \
  --json
```

Recorded response shape:

```json
{
  "coverage": {
    "completeness": "partial",
    "requested_domains": ["source"],
    "returned": 5,
    "visited": 30871
  },
  "files_scanned": 81,
  "match_count": 5,
  "graph_enrichment": {
    "status": "unavailable",
    "reason_code": "code-graph-unavailable",
    "retryable": true
  },
  "results": [
    {
      "file": "crates/tracedecay-contracts/src/code_index_freshness.rs",
      "line": 180,
      "text": "pub struct CodeIndexWorktreeFreshnessV1 {"
    }
  ],
  "truncated": true
}
```

The process exited with code 0. The source search returned real checkout data
while it kept unavailable graph enrichment typed.

## Real-project graph activation blocker

The canonical index sealed a generation for 5,090 files. Status reported the
build phase as `ready`, but `code_graph_serving` remained `pending`,
`staleness_state` remained `indexing`, and `rebuild_in_flight` remained true.
The daemon logged:

```text
verified graph head did not match the partitioned manifest; replay the exact sealed generation to repair its quarantined graph projection
```

A later search waited for the full two-minute tool deadline and exited with:

```text
reason_code=tool_dispatch_deadline_exceeded retryable=true: tool 'tracedecay_search' exceeded its absolute deadline before commit; worker settlement is Settling
```

The final status shape was:

```json
{
  "code_index_freshness": {
    "status": "warming",
    "worktree": {
      "code_graph_serving": {"state": "pending"},
      "staleness_state": "indexing",
      "rebuild_in_flight": true,
      "progress": {
        "phase": "ready",
        "completed_files": 5090,
        "total_files": 5090,
        "committed_chunks": 393200,
        "committed_imports": 83757,
        "committed_payload_bytes": 681754235
      }
    }
  },
  "graph_statistics": {
    "state": "unavailable",
    "reason": "exact_scope_generation_not_ready"
  },
  "retrieval_serving": {
    "status": "serving",
    "condition": "rebuilding",
    "freshness": "last_complete_stale"
  }
}
```

The lexical journey succeeded, but the graph journey did not converge.

## Additional CLI contract mismatch

`tracedecay init --help` advertises `--yes` for moved-store adoption. This
command:

```bash
"$BIN" init /fast/tmp/td-1281-dogfood-sol --fresh --yes
```

was rejected before initialization:

```text
Error: config error: --component, --dry-run, --yes, and --adopt are only valid with install, update-plugin, reinstall, or uninstall
```

Removing `--yes` allowed the documented `--fresh` path to complete.
