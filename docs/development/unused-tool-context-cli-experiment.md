# Unused returned tool context — CLI-call experiment

This file is a scorer experiment, not #3372 completion evidence. The
rows are constructed: live `tracedecay tool` CLI calls were recorded as
Cursor transcripts, imported with `tracedecay sessions import`, then
measured. They do not show how agents consumed returned context in
naturally captured sessions. Do not cite these unused % values on #3372
or #3373.

Raw meter JSON: `docs/development/unused-tool-context-cli-experiment.json`.

## Provenance

- This cloud environment has no operator `~/.tracedecay` store.
- 73 Cursor desktop cloud-agent transcripts on `ScriptedAlchemy/tracedecay` were scanned: **0 `tracedecay_*` MCP calls**.
- Scored rows are live `tracedecay tool` calls against four enrolled repos (`orders`, `orders-broken`, `policy`, `hooks`), recorded as Cursor transcripts, imported with `tracedecay sessions import`, then measured by `scripts/measure-unused-tool-context.py`.
- 8 sessions, **432** scored calls (**386** non-error).
- **token_count_coverage = 0%** (0/386). Stored `tool_result` facts have no `token_count` field; token columns stay `null`. Recording real counts is #3397 / draft #3400 — do not estimate here.
- Re-request after a cut is **null**: imported results do not record `cut`.

## Per-tool used / unused (bytes)

| tool | sessions | calls | errors | calls 0 used | rerequests | after cut | bytes | used bytes | unused bytes | unused % | tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|
| tracedecay_grep | 8 | 148 | 0 | 116 | 1 | null | 349391 | 74290 | 275101 | 78.7 | null |
| tracedecay_search | 8 | 108 | 0 | 83 | 4 | null | 140642 | 46757 | 93885 | 66.8 | null |
| tracedecay_callers | 8 | 35 | 16 | 19 | 0 | null | 113882 | 0 | 113882 | 100.0 | null |
| tracedecay_source_body | 4 | 19 | 0 | 19 | 0 | null | 49542 | 0 | 49542 | 100.0 | null |
| tracedecay_module_api | 4 | 4 | 0 | 4 | 0 | null | 46072 | 0 | 46072 | 100.0 | null |
| tracedecay_context | 8 | 28 | 0 | 20 | 0 | null | 43566 | 13884 | 29682 | 68.1 | null |
| tracedecay_git_hunks | 8 | 8 | 0 | 8 | 0 | null | 35550 | 0 | 35550 | 100.0 | null |
| tracedecay_files | 8 | 28 | 0 | 20 | 0 | null | 21612 | 7516 | 14096 | 65.2 | null |
| tracedecay_git_history | 4 | 4 | 0 | 4 | 0 | null | 16585 | 0 | 16585 | 100.0 | null |
| tracedecay_git_diff | 4 | 4 | 0 | 0 | 0 | null | 14365 | 14365 | 0 | 0.0 | null |
| tracedecay_git_status | 4 | 4 | 0 | 4 | 0 | null | 13385 | 0 | 13385 | 100.0 | null |
| tracedecay_hotspots | 4 | 4 | 0 | 4 | 0 | null | 6951 | 0 | 6951 | 100.0 | null |
| tracedecay_coupling | 4 | 4 | 0 | 4 | 0 | null | 1599 | 0 | 1599 | 100.0 | null |
| tracedecay_circular | 4 | 4 | 0 | 4 | 0 | null | 906 | 0 | 906 | 100.0 | null |
| tracedecay_source_read | 4 | 14 | 14 | 0 | 0 | null | 0 | 0 | 0 | - | null |
| tracedecay_impact | 4 | 16 | 16 | 0 | 0 | null | 0 | 0 | 0 | - | null |
| **all** | 8 | 432 | 46 | 309 | 5 | null | 854048 | 156812 | 697236 | 81.6 | null |

## Spot-checked examples (path/symbol keys only)

### tracedecay_callers

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/3189 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/3189 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/4665 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_circular

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/158 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/164 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/158 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_context

- **ignored** session=`unused-context-orders-broken` used_lines=0/1 bytes=0/506 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken` used_lines=0/1 bytes=0/505 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken` used_lines=0/1 bytes=0/586 rerequests=0 after_cut=None tokens=None matches=[]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=655/655 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/inventory.rs']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=973/973 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/inventory.rs']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=598/598 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/inventory.rs']]

### tracedecay_coupling

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/277 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/191 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/460 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_files

- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/578 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/492 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/531 rerequests=0 after_cut=None tokens=None matches=[]
- **used** session=`unused-context-policy` used_lines=1/1 bytes=827/827 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'work_loop']]
- **used** session=`unused-context-policy` used_lines=1/1 bytes=633/633 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'work_loop']]
- **used** session=`unused-context-policy` used_lines=1/1 bytes=779/779 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'work_loop']]

### tracedecay_git_diff

- _ignored: none in this run_
- **used** session=`unused-context-orders-wave2` used_lines=1/1 bytes=3590/3590 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'working_tree']]
- **used** session=`unused-context-orders-broken-wave2` used_lines=1/1 bytes=3596/3596 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'working_tree']]
- **used** session=`unused-context-policy-wave2` used_lines=1/1 bytes=3590/3590 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'working_tree']]

### tracedecay_git_history

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/4142 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/4162 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/4142 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_git_hunks

- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/4438 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken` used_lines=0/1 bytes=0/4445 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy` used_lines=0/1 bytes=0/4438 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_git_status

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/3345 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/3351 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/3345 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_grep

- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/179 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/179 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/2354 rerequests=0 after_cut=None tokens=None matches=[]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=2353/2353 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/orders.rs']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=810/810 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/orders.rs']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=656/656 rerequests=0 after_cut=None tokens=None matches=[['path', 'src/inventory.rs']]

### tracedecay_hotspots

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/2549 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/1566 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/1425 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_module_api

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/9933 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-broken-wave2` used_lines=0/1 bytes=0/2309 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-policy-wave2` used_lines=0/1 bytes=0/16345 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

### tracedecay_search

- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/223 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/223 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders` used_lines=0/1 bytes=0/223 rerequests=0 after_cut=None tokens=None matches=[]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=1405/1405 rerequests=1 after_cut=None tokens=None matches=[['symbol', 'exact_message']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=1984/1984 rerequests=1 after_cut=None tokens=None matches=[['symbol', 'exact_message']]
- **used** session=`unused-context-orders` used_lines=1/1 bytes=2554/2554 rerequests=0 after_cut=None tokens=None matches=[['symbol', 'exact_message']]

### tracedecay_source_body

- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/2585 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/2662 rerequests=0 after_cut=None tokens=None matches=[]
- **ignored** session=`unused-context-orders-wave2` used_lines=0/1 bytes=0/2632 rerequests=0 after_cut=None tokens=None matches=[]
- _used: none in this run_

