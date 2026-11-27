# MCP probe matrix

The probe starts a fresh `tracedecay serve` MCP server over stdio and runs
every read-only tool against a set of real repos. The results table records
errors, timeouts, empty results, and slow calls for each tool and repo.

Rerun the probe after adding language support, adding a tool, or refactoring.
Check the flagged cells for regressions. The probe logs the duration of each
call, so you can compare versions using the same repos.

## Layout

```
scripts/mcp_probe/
├── README.md
├── repos.toml             Lists repo paths and their probe sets.
├── probe.py               Starts MCP, calls tools, and logs the results.
├── isolated.py            Retries each tool with a fresh server.
├── build_matrix.py        Reads the log and writes a Markdown table.
└── tools/
    └── rust.py            Defines five Rust queries per tool.
```

`tools/` is the place to add language-specific probe inputs (Python, Go, …).
Each module exposes a `probes_for(discovered)` function that returns
`{tool_name: [args_dict, …]}`.

## Quick run

```sh
# 1. Build the release binary (driver uses bazel-bin/crates/tracedecay-cli/tracedecay)
bazel build --config=release //crates/tracedecay-cli:tracedecay

# 2. Run the matrix
python3 scripts/mcp_probe/probe.py
python3 scripts/mcp_probe/build_matrix.py > /tmp/matrix.md
```

The driver writes one tab-separated row per call to `/tmp/tracedecay_matrix.log`.
The columns are `repo`, `tool`, `query_json`, `status`, and `detail`. The
status is one of `OK`, `EMPTY`, `ERROR`, `TIMEOUT`, `BAD`, or `SLOW`.

## Cascade caveat

The MCP server is single-threaded over stdio. If a tool times out, its late
response can put clients that don't match response IDs out of sync. They may
mistake the late response for the next call's response and report more
timeouts. `probe.py` matches IDs when possible. If a tool shows a real
timeout, retry it with `isolated.py`, which starts a fresh server for each tool.

## Adding a new repo

Append to `repos.toml`:

```toml
[[repos]]
name = "polkadot"
path = "/Volumes/home_ext1/Src/0parity/polkadot-sdk"
languages = ["rust"]
```

`languages` selects which probe modules (`tools/<lang>.py`) contribute their
query set for that repo.

## Adding a new language

Add `tools/<lang>.py` with a `probes_for(discovered)` function. Then list
`<lang>` in any repo's `languages` array.
